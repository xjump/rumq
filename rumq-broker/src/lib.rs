#![recursion_limit="512"]

#[macro_use]
extern crate log;

use derive_more::From;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::task;
use tokio::time::{self, Elapsed};
use tokio_rustls::rustls::TLSError;

use serde::Deserialize;

use std::io;
use std::sync::Arc;
use std::time::Duration;
use std::cell::RefCell;
use std::rc::Rc;

mod connection;
mod httppush;
mod httpserver;
mod router;
mod state;

#[derive(From, Debug)]
pub enum Error {
    Io(io::Error),
    Mqtt(rumq_core::Error),
    Timeout(Elapsed),
    State(state::Error),
    Tls(TLSError),
    NoServerCert,
    NoServerPrivateKey,
    NoCAFile,
    NoServerCertFile,
    NoServerKeyFile,
    Disconnected,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    servers:    Vec<ServerSettings>,
    httppush:   HttpPush,
    httpserver: HttpServer,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HttpPush {
    url:   String,
    topic: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HttpServer {
    port: u16,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerSettings {
    pub port: u16,
    pub connection_timeout_ms: u16,
    pub max_client_id_len: usize,
    pub max_connections: usize,
    /// Throughput from cloud to device
    pub max_cloud_to_device_throughput: usize,
    /// Throughput from device to cloud
    pub max_device_to_cloud_throughput: usize,
    /// Minimum delay time between consecutive outgoing packets
    pub max_incoming_messages_per_sec: usize,
    pub disk_persistence: bool,
    pub disk_retention_size: usize,
    pub disk_retention_time_sec: usize,
    pub auto_save_interval_sec: u16,
    pub max_packet_size: usize,
    pub max_inflight_queue_size: usize,
    pub ca_path: Option<String>,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

pub async fn accept_loop(config: Arc<ServerSettings>, router: Rc<RefCell<router::Router>>) -> Result<(), Error> {
    let local = task::LocalSet::new();
    let local = Arc::new(local);
    let addr = format!("0.0.0.0:{}", config.port);
    let connection_config = config.clone();

    info!("Waiting for connections on {}", addr);
    // eventloop which accepts connections
    let mut listener = TcpListener::bind(addr).await?;
    local.run_until(async move {
        loop {
            let (stream, addr) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    error!("Tcp connection error = {:?}", e);
                    break;
                }
            };

            info!("Accepting from: {}", addr);

            let config = connection_config.clone();
            let router = router.clone();
            task::spawn_local(eventloop(config, router, stream));
            time::delay_for(Duration::from_millis(10)).await;
        }

        Ok::<_, ()>(())
    }).await.unwrap();

    Ok(())
}
async fn eventloop(config: Arc<ServerSettings>, router: Rc<RefCell<router::Router>>, stream: impl Network) {
    match connection::eventloop(config, router, stream).await {
        Ok(id) => info!("Connection eventloop done!!. Id = {:?}", id),
        Err(e) => error!("Connection eventloop error = {:?}", e),
    }
}

#[tokio::main]
pub async fn start(config: Config) {
    let router = router::Router::new();
    let router = Rc::new(RefCell::new(router));

    let server = config.servers.get(0).unwrap().clone();
    let config = Arc::new(server);
    let fut = accept_loop(config, router.clone());
    fut.await.unwrap();
}

pub trait Network: AsyncWrite + AsyncRead + Unpin + Send {}
impl<T> Network for T where T: AsyncWrite + AsyncRead + Unpin + Send {}

#[cfg(test)]
mod test {
    #[test]
    fn accept_loop_rate_limits_incoming_connections() {}

    #[test]
    fn accept_loop_should_not_allow_more_than_maximum_connections() {}

    #[test]
    fn accept_loop_should_accept_new_connection_when_a_client_disconnects_after_max_connections() {}

    #[test]
    fn client_loop_should_error_if_connect_packet_is_not_received_in_time() {}
}
