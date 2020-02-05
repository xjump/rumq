#![recursion_limit="512"]

#[macro_use]
extern crate log;

use derive_more::From;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{channel, Sender, Receiver};
use tokio::task;
use tokio::task::LocalSet;
use tokio::time::{self, Elapsed};
use tokio_rustls::rustls::internal::pemfile::{certs, rsa_private_keys};
use tokio_rustls::rustls::TLSError;
use tokio_rustls::rustls::{AllowAnyAuthenticatedClient, NoClientAuth, RootCertStore, ServerConfig};
use tokio_rustls::TlsAcceptor;

use serde::Deserialize;

use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;
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

async fn tls_connection<P: AsRef<Path>>(ca_path: Option<P>, cert_path: P, key_path: P) -> Result<TlsAcceptor, Error> {
    // client authentication with a CA. CA isn't required otherwise
    let mut server_config = if let Some(ca_path) = ca_path {
        let mut root_cert_store = RootCertStore::empty();
        root_cert_store.add_pem_file(&mut BufReader::new(File::open(ca_path)?)).map_err(|_| Error::NoCAFile)?;
        ServerConfig::new(AllowAnyAuthenticatedClient::new(root_cert_store))
    } else {
        ServerConfig::new(NoClientAuth::new())
    };

    let certs = certs(&mut BufReader::new(File::open(cert_path)?)).map_err(|_| Error::NoServerCertFile)?;
    let mut keys = rsa_private_keys(&mut BufReader::new(File::open(key_path)?)).map_err(|_| Error::NoServerKeyFile)?;

    server_config.set_single_cert(certs, keys.remove(0))?;
    let acceptor = TlsAcceptor::from(Arc::new(server_config));
    Ok(acceptor)
}

pub async fn accept_loop(config: Arc<ServerSettings>, router: Rc<RefCell<router::Router>>) -> Result<(), Error> {
    let local = task::LocalSet::new();
    let local = Arc::new(local);
    let addr = format!("0.0.0.0:{}", config.port);
    let connection_config = config.clone();

    let acceptor = if let Some(cert_path) = config.cert_path.clone() {
        let key_path = config.key_path.clone().ok_or(Error::NoServerPrivateKey)?;
        Some(tls_connection(config.ca_path.clone(), cert_path, key_path).await?)
    } else {
        None
    };

    info!("Waiting for connections on {}", addr);
    // eventloop which accepts connections
    let mut listener = TcpListener::bind(addr).await?;
    local.run_until(async move {
        loop {
            let (stream, addr) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    error!("Tcp connection error = {:?}", e);
                    continue;
                }
            };

            info!("Accepting from: {}", addr);

            let config = connection_config.clone();
            let router = router.clone();
            if let Some(acceptor) = &acceptor {
                unimplemented!()
            } else {
                task::spawn_local(eventloop(config, router, stream));
            };

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
