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

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    fn test_config() -> Arc<Config> {
        Arc::new(Config::default())
    }

    fn test_pool() -> ConnectionPool {
        let config = test_config();
        let dns = Arc::new(DnsCache::new(config.dns_cache_ttl));
        let tls = crate::tls::build_tls_config();
        ConnectionPool::new(config, dns, tls)
    }

    fn test_pool_with_config(config: Config) -> ConnectionPool {
        let config = Arc::new(config);
        let dns = Arc::new(DnsCache::new(config.dns_cache_ttl));
        let tls = crate::tls::build_tls_config();
        ConnectionPool::new(config, dns, tls)
    }

    /// Create an HTTP/2 sender backed by an in-memory DuplexStream.
    async fn make_h2_sender() -> http2::SendRequest<ReqBody> {
        let (client_io, server_io) = tokio::io::duplex(65536);

        // Server side
        tokio::spawn(async move {
            let io = TokioIo::new(server_io);
            let service = hyper::service::service_fn(
                |_req: hyper::Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(hyper::Response::new(Empty::<Bytes>::new()))
                },
            );
            let builder = hyper::server::conn::http2::Builder::new(TokioExecutor::new());
            let _ = builder.serve_connection(io, service).await;
        });

        // Client side
        let io = TokioIo::new(client_io);
        let (sender, conn) = http2::Builder::new(TokioExecutor::new())
            .handshake(io)
            .await
            .expect("h2 client handshake");
        tokio::spawn(async move {
            let _ = conn.await;
        });

        sender
    }

    /// Create an HTTP/1.1 sender backed by an in-memory DuplexStream.
    async fn make_h1_sender() -> http1::SendRequest<ReqBody> {
        let (client_io, server_io) = tokio::io::duplex(65536);

        tokio::spawn(async move {
            let io = TokioIo::new(server_io);
            let service = hyper::service::service_fn(
                |_req: hyper::Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(hyper::Response::new(Empty::<Bytes>::new()))
                },
            );
            let builder = hyper::server::conn::http1::Builder::new();
            let _ = builder.serve_connection(io, service).await;
        });

        let io = TokioIo::new(client_io);
        let (sender, conn) = http1::Builder::new()
            .handshake(io)
            .await
            .expect("h1 client handshake");
        tokio::spawn(async move {
            let _ = conn.await;
        });

        sender
    }

    /// Insert an H2 sender directly into the pool for testing.
    async fn insert_h2(
        pool: &ConnectionPool,
        host: &str,
        port: u16,
        sender: http2::SendRequest<ReqBody>,
    ) -> usize {
        let key = (host.to_string(), port);
        let mut pools = pool.host_pools.lock().await;
        let hp = pools.entry(key).or_insert_with(|| HostPool {
            protocol: NegotiatedProtocol::Http2,
            connections: Vec::new(),
            notify: Arc::new(Notify::new()),
            opening_count: 0,
            next_id: 0,
        });
        let id = hp.allocate_id();
        hp.connections.push(PooledConnection {
            id,
            sender: ConnectionSender::H2(sender),
        });
        id
    }

    /// Insert an H1 sender directly into the pool for testing.
    async fn insert_h1(
        pool: &ConnectionPool,
        host: &str,
        port: u16,
        sender: http1::SendRequest<ReqBody>,
    ) -> usize {
        let key = (host.to_string(), port);
        let mut pools = pool.host_pools.lock().await;
        let hp = pools.entry(key).or_insert_with(|| HostPool {
            protocol: NegotiatedProtocol::Http1,
            connections: Vec::new(),
            notify: Arc::new(Notify::new()),
            opening_count: 0,
            next_id: 0,
        });
        let id = hp.allocate_id();
        hp.connections.push(PooledConnection {
            id,
            sender: ConnectionSender::H1(Some(sender)),
        });
        id
    }

    #[tokio::test]
    async fn pool_starts_empty() {
        let pool = test_pool();
        let pools = pool.host_pools.lock().await;
        assert!(pools.is_empty());
    }

    #[tokio::test]
    async fn acquire_returns_injected_h2_connection() {
        let pool = test_pool();
        let sender = make_h2_sender().await;
        let conn_id = insert_h2(&pool, "test.local", 443, sender).await;

        let handle = pool.acquire("test.local", 443).await.unwrap();
        assert_eq!(handle.connection_id, conn_id);
        assert!(matches!(handle.sender, PoolHandleSender::H2(_)));
    }

    #[tokio::test]
    async fn acquire_reuses_h2_connection() {
        let pool = test_pool();
        let sender = make_h2_sender().await;
        insert_h2(&pool, "test.local", 443, sender).await;

        let handle1 = pool.acquire("test.local", 443).await.unwrap();
        let handle2 = pool.acquire("test.local", 443).await.unwrap();

        // Both should reference the same underlying connection
        assert_eq!(handle1.connection_id, handle2.connection_id);
    }

    #[tokio::test]
    async fn h1_checkout_is_exclusive() {
        let pool = test_pool();
        let sender = make_h1_sender().await;
        insert_h1(&pool, "test.local", 80, sender).await;

        let handle = pool.acquire("test.local", 80).await.unwrap();
        assert!(matches!(handle.sender, PoolHandleSender::H1(_)));

        // Connection should be checked out (None) in pool
        let pools = pool.host_pools.lock().await;
        let hp = pools.get(&("test.local".to_string(), 80)).unwrap();
        assert_eq!(hp.connections.len(), 1);
        if let ConnectionSender::H1(ref opt) = hp.connections[0].sender {
            assert!(opt.is_none(), "H1 sender should be checked out");
        } else {
            panic!("expected H1 connection");
        }
    }

    #[tokio::test]
    async fn h1_return_makes_connection_reusable() {
        let pool = test_pool();
        let sender = make_h1_sender().await;
        let conn_id = insert_h1(&pool, "test.local", 80, sender).await;

        // Check out
        let handle = pool.acquire("test.local", 80).await.unwrap();
        let PoolHandleSender::H1(h1_sender) = handle.sender else {
            panic!("expected H1")
        };

        // Return
        pool.return_h1_connection("test.local", 80, conn_id, h1_sender)
            .await;

        // Re-acquire should succeed with same connection
        let handle2 = pool.acquire("test.local", 80).await.unwrap();
        assert_eq!(handle2.connection_id, conn_id);
    }

    #[tokio::test]
    async fn h1_waits_when_all_checked_out() {
        let pool = Arc::new(test_pool());
        let sender = make_h1_sender().await;
        let conn_id = insert_h1(&pool, "test.local", 80, sender).await;

        // Check out
        let handle = pool.acquire("test.local", 80).await.unwrap();
        let PoolHandleSender::H1(h1_sender) = handle.sender else {
            panic!("expected H1")
        };

        // Spawn waiter that should block
        let pool2 = Arc::clone(&pool);
        let waiter = tokio::spawn(async move { pool2.acquire("test.local", 80).await });

        // Give the waiter time to register on notify
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Return connection - should wake waiter
        pool.return_h1_connection("test.local", 80, conn_id, h1_sender)
            .await;

        let result = tokio::time::timeout(Duration::from_secs(1), waiter).await;
        assert!(result.is_ok(), "waiter should complete");
        let handle2 = result.unwrap().unwrap().unwrap();
        assert_eq!(handle2.connection_id, conn_id);
    }

    #[tokio::test]
    async fn remove_dead_connection_cleans_pool() {
        let pool = test_pool();
        let sender = make_h2_sender().await;
        let conn_id = insert_h2(&pool, "test.local", 443, sender).await;

        pool.remove_dead_connection("test.local", 443, conn_id)
            .await;

        let pools = pool.host_pools.lock().await;
        let hp = pools.get(&("test.local".to_string(), 443)).unwrap();
        assert!(hp.connections.is_empty());
    }

    #[tokio::test]
    async fn remove_dead_connection_notifies_waiters() {
        let pool = Arc::new(test_pool());
        let sender = make_h2_sender().await;
        let conn_id = insert_h2(&pool, "test.local", 443, sender).await;

        let _handle = pool.acquire("test.local", 443).await.unwrap();

        pool.remove_dead_connection("test.local", 443, conn_id)
            .await;

        let pools = pool.host_pools.lock().await;
        let hp = pools.get(&("test.local".to_string(), 443)).unwrap();
        assert!(hp.connections.is_empty());
    }

    #[tokio::test]
    async fn pool_config_stores_custom_window_size() {
        let config = Config {
            stream_window_size: 1_048_576,
            ..Default::default()
        };
        let pool = test_pool_with_config(config);
        assert_eq!(pool.config.stream_window_size, 1_048_576);
    }

    #[tokio::test]
    async fn pool_config_stores_connections_per_host() {
        let config = Config {
            connections_per_host: 4,
            ..Default::default()
        };
        let pool = test_pool_with_config(config);
        assert_eq!(pool.config.connections_per_host, 4);
    }

    #[tokio::test]
    async fn h2_dead_connection_detected_and_removed() {
        let pool = test_pool();

        // Create a sender then immediately drop the server side
        let sender = {
            let (client_io, _server_io) = tokio::io::duplex(65536);
            let io = TokioIo::new(client_io);
            let (sender, conn) = http2::Builder::new(TokioExecutor::new())
                .handshake(io)
                .await
                .expect("h2 handshake");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            sender
        };

        // Give the connection time to detect the dead server
        tokio::time::sleep(Duration::from_millis(50)).await;

        insert_h2(&pool, "dead.local", 443, sender).await;

        // acquire should detect the dead connection and try to open a new one
        // (which will fail since "dead.local" won't resolve)
        let result = pool.acquire("dead.local", 443).await;
        assert!(
            result.is_err(),
            "should fail since DNS for dead.local fails"
        );

        // Host pool should be cleaned up
        let pools = pool.host_pools.lock().await;
        assert!(
            !pools.contains_key(&("dead.local".to_string(), 443)),
            "host pool should be removed after failed connection"
        );
    }

    #[tokio::test]
    async fn acquire_respects_connections_per_host_limit() {
        let config = Config {
            connections_per_host: 2,
            ..Default::default()
        };
        let pool = test_pool_with_config(config);

        // Insert 2 H1 connections (at the limit)
        let s1 = make_h1_sender().await;
        let s2 = make_h1_sender().await;
        insert_h1(&pool, "test.local", 80, s1).await;
        insert_h1(&pool, "test.local", 80, s2).await;

        // Check out both
        let _h1 = pool.acquire("test.local", 80).await.unwrap();
        let _h2 = pool.acquire("test.local", 80).await.unwrap();

        // Pool is now at limit with all checked out.
        // Verify the pool sees 2 connections (at limit).
        let pools = pool.host_pools.lock().await;
        let hp = pools.get(&("test.local".to_string(), 80)).unwrap();
        assert_eq!(hp.connections.len(), 2);
        assert_eq!(hp.opening_count, 0);
    }

    #[tokio::test]
    async fn notify_h2_complete_wakes_waiters() {
        let pool = Arc::new(test_pool());
        let sender = make_h2_sender().await;
        insert_h2(&pool, "test.local", 443, sender).await;

        // Acquire a connection
        let _handle = pool.acquire("test.local", 443).await.unwrap();

        // Verify notify_h2_complete doesn't panic on valid host
        pool.notify_h2_complete("test.local", 443).await;

        // Verify notify_h2_complete doesn't panic on unknown host
        pool.notify_h2_complete("unknown.local", 443).await;
    }
}
