use std::collections::HashMap;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Empty;
use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tokio_rustls::TlsConnector;
use tracing::Instrument;

use crate::config::Config;
use crate::dns::DnsCache;
use crate::types::DownloadError;

/// Body type for outgoing requests (empty for GET requests).
pub(crate) type ReqBody = Empty<Bytes>;

/// Composite key for pool lookups.
type HostKey = (String, u16);

/// Maximum iterations in the acquire loop before giving up.
const MAX_ACQUIRE_ATTEMPTS: usize = 32;

/// Timeout for waiting on a notify signal before retrying.
const NOTIFY_WAIT_TIMEOUT: Duration = Duration::from_millis(250);

/// Negotiated protocol after TLS handshake.
#[derive(Debug, Clone, Copy, PartialEq)]
enum NegotiatedProtocol {
    Http2,
    Http1,
}

/// Internal connection sender, differentiated by protocol.
enum ConnectionSender {
    /// HTTP/2 sender - Clone-able, supports multiplexed streams.
    H2(http2::SendRequest<ReqBody>),
    /// HTTP/1.1 sender - Option for checkout semantics.
    /// None = checked out, Some = idle.
    H1(Option<http1::SendRequest<ReqBody>>),
}

/// A single pooled connection with a unique ID.
struct PooledConnection {
    id: usize,
    sender: ConnectionSender,
}

/// Per-host connection tracking.
struct HostPool {
    protocol: NegotiatedProtocol,
    connections: Vec<PooledConnection>,
    notify: Arc<Notify>,
    opening_count: usize,
    next_id: usize,
}

impl HostPool {
    fn total_count(&self) -> usize {
        self.connections.len() + self.opening_count
    }

    fn allocate_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// The connection pool, shared across all download tasks.
pub(crate) struct ConnectionPool {
    tls_config: Arc<rustls::ClientConfig>,
    dns_cache: Arc<DnsCache>,
    host_pools: Mutex<HashMap<HostKey, HostPool>>,
    config: Arc<Config>,
    plain_mode: bool,
}

/// Handle returned by `acquire()`, wrapping a sender ready for requests.
pub(crate) struct PoolHandle {
    pub sender: PoolHandleSender,
    pub connection_id: usize,
}

/// Protocol-specific sender inside a PoolHandle.
pub(crate) enum PoolHandleSender {
    H2(http2::SendRequest<ReqBody>),
    H1(http1::SendRequest<ReqBody>),
}

/// Result of opening a new connection (before pool insertion).
enum OpenedConnection {
    H2(http2::SendRequest<ReqBody>),
    H1(http1::SendRequest<ReqBody>),
}

/// Action determined by pool state analysis.
enum AcquireAction {
    ReturnExisting(PoolHandle),
    OpenNew { is_first: bool },
    Wait(Arc<Notify>),
}

/// HTTP/2 sender readiness status.
enum H2Status {
    Ready,
    AtCapacity,
    Dead,
}

/// Noop waker for non-blocking poll_ready checks.
struct NoopWake;
impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

/// Static noop waker to avoid per-call Arc allocation.
fn noop_waker() -> Waker {
    use std::sync::LazyLock;
    static WAKER: LazyLock<Waker> = LazyLock::new(|| Waker::from(Arc::new(NoopWake)));
    WAKER.clone()
}

/// Check HTTP/2 sender readiness without blocking.
fn check_h2_ready(sender: &mut http2::SendRequest<ReqBody>) -> H2Status {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    match sender.poll_ready(&mut cx) {
        Poll::Ready(Ok(())) => H2Status::Ready,
        Poll::Ready(Err(_)) => H2Status::Dead,
        Poll::Pending => H2Status::AtCapacity,
    }
}

impl ConnectionPool {
    /// Create a new connection pool.
    pub(crate) fn new(
        config: Arc<Config>,
        dns_cache: Arc<DnsCache>,
        tls_config: Arc<rustls::ClientConfig>,
    ) -> Self {
        ConnectionPool {
            tls_config,
            dns_cache,
            host_pools: Mutex::new(HashMap::new()),
            config,
            plain_mode: false,
        }
    }

    /// Create a connection pool that uses plain TCP (no TLS). For testing only.
    #[doc(hidden)]
    pub(crate) fn new_plain(
        config: Arc<Config>,
        dns_cache: Arc<DnsCache>,
        tls_config: Arc<rustls::ClientConfig>,
    ) -> Self {
        ConnectionPool {
            tls_config,
            dns_cache,
            host_pools: Mutex::new(HashMap::new()),
            config,
            plain_mode: true,
        }
    }

    /// Acquire a connection handle for the given host.
    ///
    /// Returns an existing connection if available, or opens a new one.
    /// Blocks if all connections are busy and the pool is at capacity.
    pub(crate) async fn acquire(&self, host: &str, port: u16) -> Result<PoolHandle, DownloadError> {
        let key = (host.to_string(), port);
        for _ in 0..MAX_ACQUIRE_ATTEMPTS {
            let action = {
                let mut pools = self.host_pools.lock().await;
                self.determine_action(&mut pools, &key, host, port)
            };
            match action {
                AcquireAction::ReturnExisting(handle) => return Ok(handle),
                AcquireAction::OpenNew { is_first } => {
                    return self.open_and_insert(host, port, &key, is_first).await;
                }
                AcquireAction::Wait(notify) => {
                    // Use timeout so HTTP/2 stream capacity recovery is detected
                    // even without explicit notification.
                    let _ = tokio::time::timeout(NOTIFY_WAIT_TIMEOUT, notify.notified()).await;
                }
            }
        }
        Err(DownloadError::ConnectionFailed(format!(
            "pool acquire timeout for {host}:{port} after {MAX_ACQUIRE_ATTEMPTS} attempts"
        )))
    }

    /// Notify the pool that an HTTP/2 stream has completed.
    ///
    /// Called by the transport layer after each HTTP/2 request finishes,
    /// so that tasks waiting for stream capacity can be woken.
    pub(crate) async fn notify_h2_complete(&self, host: &str, port: u16) {
        let key = (host.to_string(), port);
        let pools = self.host_pools.lock().await;
        if let Some(hp) = pools.get(&key) {
            hp.notify.notify_one();
        }
    }

    /// Return an HTTP/1.1 sender to the pool after use.
    pub(crate) async fn return_h1_connection(
        &self,
        host: &str,
        port: u16,
        connection_id: usize,
        sender: http1::SendRequest<ReqBody>,
    ) {
        let key = (host.to_string(), port);
        let mut pools = self.host_pools.lock().await;
        if let Some(hp) = pools.get_mut(&key) {
            for conn in &mut hp.connections {
                if conn.id == connection_id {
                    if let ConnectionSender::H1(ref mut opt) = conn.sender {
                        *opt = Some(sender);
                    }
                    break;
                }
            }
            hp.notify.notify_one();
        }
    }

    /// Remove a dead connection. Called by transport layer on connection errors.
    pub(crate) async fn remove_dead_connection(&self, host: &str, port: u16, connection_id: usize) {
        let key = (host.to_string(), port);
        let mut pools = self.host_pools.lock().await;
        if let Some(hp) = pools.get_mut(&key) {
            hp.connections.retain(|c| c.id != connection_id);
            tracing::warn!("dead connection removed for {host}:{port}");
            hp.notify.notify_one();
        }
    }

    /// Analyze pool state and determine the acquire action.
    fn determine_action(
        &self,
        pools: &mut HashMap<HostKey, HostPool>,
        key: &HostKey,
        host: &str,
        port: u16,
    ) -> AcquireAction {
        if let Some(hp) = pools.get_mut(key) {
            match hp.protocol {
                NegotiatedProtocol::Http2 => self.h2_action(hp, host, port),
                NegotiatedProtocol::Http1 => self.h1_action(hp, host, port),
            }
        } else {
            pools.insert(
                key.clone(),
                HostPool {
                    protocol: NegotiatedProtocol::Http2, // updated after handshake
                    connections: Vec::new(),
                    notify: Arc::new(Notify::new()),
                    opening_count: 1,
                    next_id: 0,
                },
            );
            AcquireAction::OpenNew { is_first: true }
        }
    }

    fn h2_action(&self, hp: &mut HostPool, host: &str, port: u16) -> AcquireAction {
        // First pass: remove dead connections
        let initial_count = hp.connections.len();
        hp.connections.retain(|conn| {
            if let ConnectionSender::H2(ref sender) = conn.sender {
                let mut clone = sender.clone();
                !matches!(check_h2_ready(&mut clone), H2Status::Dead)
            } else {
                true
            }
        });
        let removed = initial_count - hp.connections.len();
        if removed > 0 {
            tracing::warn!("removed {removed} dead connection(s) for {host}:{port}");
        }

        // Second pass: find a ready connection
        for conn in &mut hp.connections {
            if let ConnectionSender::H2(ref mut sender) = conn.sender {
                if matches!(check_h2_ready(sender), H2Status::Ready) {
                    tracing::debug!("pool hit for {host}:{port}, reusing HTTP/2 connection");
                    return AcquireAction::ReturnExisting(PoolHandle {
                        sender: PoolHandleSender::H2(sender.clone()),
                        connection_id: conn.id,
                    });
                }
            }
        }

        self.scale_or_wait(hp, host, port)
    }

    fn h1_action(&self, hp: &mut HostPool, host: &str, port: u16) -> AcquireAction {
        for conn in &mut hp.connections {
            if let ConnectionSender::H1(ref mut opt) = conn.sender {
                if let Some(sender) = opt.take() {
                    tracing::debug!("pool hit for {host}:{port}, reusing HTTP/1.1 connection");
                    return AcquireAction::ReturnExisting(PoolHandle {
                        sender: PoolHandleSender::H1(sender),
                        connection_id: conn.id,
                    });
                }
            }
        }

        self.scale_or_wait(hp, host, port)
    }

    fn scale_or_wait(&self, hp: &mut HostPool, host: &str, port: u16) -> AcquireAction {
        if hp.total_count() < self.config.connections_per_host {
            hp.opening_count += 1;
            tracing::debug!(
                "opening new connection to {host}:{port}, current: {}",
                hp.connections.len()
            );
            AcquireAction::OpenNew { is_first: false }
        } else {
            AcquireAction::Wait(hp.notify.clone())
        }
    }

    /// Open a new connection and insert it into the pool.
    async fn open_and_insert(
        &self,
        host: &str,
        port: u16,
        key: &HostKey,
        is_first: bool,
    ) -> Result<PoolHandle, DownloadError> {
        match self.open_connection(host, port).await {
            Ok(opened) => {
                let mut pools = self.host_pools.lock().await;
                let Some(hp) = pools.get_mut(key) else {
                    return Err(DownloadError::ConnectionFailed(format!(
                        "pool entry for {host}:{port} disappeared during connection"
                    )));
                };
                hp.opening_count -= 1;

                if is_first {
                    hp.protocol = match &opened {
                        OpenedConnection::H2(_) => NegotiatedProtocol::Http2,
                        OpenedConnection::H1(_) => NegotiatedProtocol::Http1,
                    };
                }

                let id = hp.allocate_id();
                let (handle_sender, pool_sender) = match opened {
                    OpenedConnection::H2(sender) => {
                        let cloned = sender.clone();
                        (PoolHandleSender::H2(cloned), ConnectionSender::H2(sender))
                    }
                    OpenedConnection::H1(sender) => (
                        PoolHandleSender::H1(sender),
                        ConnectionSender::H1(None), // checked out
                    ),
                };

                hp.connections.push(PooledConnection {
                    id,
                    sender: pool_sender,
                });

                Ok(PoolHandle {
                    sender: handle_sender,
                    connection_id: id,
                })
            }
            Err(e) => {
                let mut pools = self.host_pools.lock().await;
                if let Some(hp) = pools.get_mut(key) {
                    hp.opening_count -= 1;
                    if hp.connections.is_empty() && hp.opening_count == 0 {
                        pools.remove(key);
                    } else {
                        hp.notify.notify_one();
                    }
                }
                Err(e)
            }
        }
    }

    /// Establish a new TCP + TLS + HTTP connection.
    async fn open_connection(
        &self,
        host: &str,
        port: u16,
    ) -> Result<OpenedConnection, DownloadError> {
        let span = tracing::debug_span!("connect", host = %host, port = %port);
        self.open_connection_inner(host, port)
            .instrument(span)
            .await
    }

    async fn open_connection_inner(
        &self,
        host: &str,
        port: u16,
    ) -> Result<OpenedConnection, DownloadError> {
        // 1. DNS resolution
        let addrs = self.dns_cache.resolve(host, port).await?;

        // 2. TCP connect with timeout -- try all addresses
        let tcp_stream = {
            let mut last_err = None;
            let mut connected = None;

            for addr in &addrs {
                match tokio::time::timeout(self.config.connect_timeout, TcpStream::connect(addr))
                    .await
                {
                    Ok(Ok(stream)) => {
                        connected = Some(stream);
                        break;
                    }
                    Ok(Err(e)) => {
                        last_err = Some(e.to_string());
                    }
                    Err(_) => {
                        last_err = Some(format!("connect to {addr} timed out"));
                    }
                }
            }

            connected.ok_or_else(|| {
                DownloadError::ConnectionFailed(
                    last_err.unwrap_or_else(|| "no addresses to connect to".into()),
                )
            })?
        };

        // Plain mode: skip TLS, use HTTP/1.1 directly
        if self.plain_mode {
            let io = TokioIo::new(tcp_stream);
            let (sender, conn) = http1::Builder::new()
                .handshake(io)
                .await
                .map_err(|e| DownloadError::ConnectionFailed(format!("HTTP/1.1 handshake: {e}")))?;
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    tracing::error!("HTTP/1.1 connection driver error: {e}");
                }
            });
            return Ok(OpenedConnection::H1(sender));
        }

        // 3. TLS handshake
        let connector = TlsConnector::from(self.tls_config.clone());
        let server_name = rustls_pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| DownloadError::TlsError(format!("invalid server name: {e}")))?;

        let tls_stream = connector
            .connect(server_name, tcp_stream)
            .await
            .map_err(|e| DownloadError::TlsError(e.to_string()))?;

        // 4. Protocol detection via ALPN
        let protocol = match tls_stream.get_ref().1.alpn_protocol() {
            Some(b"h2") => NegotiatedProtocol::Http2,
            _ => {
                tracing::warn!("HTTP/1.1 fallback for {host}:{port}");
                NegotiatedProtocol::Http1
            }
        };

        // 5. HTTP handshake
        let io = TokioIo::new(tls_stream);
        match protocol {
            NegotiatedProtocol::Http2 => {
                let mut builder = http2::Builder::new(TokioExecutor::new());
                builder
                    .timer(TokioTimer::new())
                    .initial_stream_window_size(self.config.stream_window_size)
                    .adaptive_window(true)
                    .keep_alive_interval(Some(Duration::from_secs(10)))
                    .keep_alive_timeout(Duration::from_secs(20));

                let (sender, conn) = builder.handshake(io).await.map_err(|e| {
                    DownloadError::ConnectionFailed(format!("HTTP/2 handshake: {e}"))
                })?;

                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        tracing::error!("HTTP/2 connection driver error: {e}");
                    }
                });

                Ok(OpenedConnection::H2(sender))
            }
            NegotiatedProtocol::Http1 => {
                let (sender, conn) = http1::Builder::new().handshake(io).await.map_err(|e| {
                    DownloadError::ConnectionFailed(format!("HTTP/1.1 handshake: {e}"))
                })?;

                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        tracing::error!("HTTP/1.1 connection driver error: {e}");
                    }
                });

                Ok(OpenedConnection::H1(sender))
            }
        }
    }
}
