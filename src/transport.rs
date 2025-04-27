use crate::logging::{GenericError, SimpleError};
use crate::tls::TlsManager;
use arti_client::TorClient;
use async_trait::async_trait;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::HeaderValue;
use hyper::http::response::Builder;
use hyper::upgrade::Upgraded;
use hyper::{header, Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use log::{debug, error};
use std::sync::Arc;
use tokio::io::{copy, copy_bidirectional, sink, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tor_rtcompat::PreferredRuntime;
use web3_resolver::models::ResolvedDomainData;

#[async_trait]
pub trait ProxyTransport {
    async fn connect_url(&self, uri: Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError>;
    async fn get_url(&self, uri: &Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError>;
}

pub struct TorProxyTransport{
    tor_client: Arc<OnceCell<Arc<TorClient<PreferredRuntime>>>>
}

impl TorProxyTransport {
    pub fn new(tor_client: Arc<OnceCell<Arc<TorClient<PreferredRuntime>>>>) -> Self {
        Self {
            tor_client
        }
    }
}

pub struct IpProxyTransport {
}

impl IpProxyTransport {
    pub fn new() -> Self {
        Self {
        }
    }
}

#[derive(Clone)]
pub struct OnchainProxyTransport {
    tls_manager: Arc<TlsManager>,
}

impl OnchainProxyTransport {
    pub fn new(tls_manager: Arc<TlsManager>) -> Self {
        Self {
            tls_manager,
        }
    }
}

pub struct SimpleProxyTransport {
    tor_client: Arc<OnceCell<Arc<TorClient<PreferredRuntime>>>>
}

impl SimpleProxyTransport {
    pub fn new(tor_client: Arc<OnceCell<Arc<TorClient<PreferredRuntime>>>>) -> Self {
        Self {
            tor_client
        }
    }
}

#[derive(Clone)]
pub struct RedirectProxyTransport {
    tls_manager: Arc<TlsManager>,
    tor_client: Arc<OnceCell<Arc<TorClient<PreferredRuntime>>>>
}

impl RedirectProxyTransport {
    pub fn new(tls_manager: Arc<TlsManager>, tor_client: Arc<OnceCell<Arc<TorClient<PreferredRuntime>>>>) -> Self {
        Self {
            tls_manager,
            tor_client
        }
    }
}

#[async_trait]
impl ProxyTransport for SimpleProxyTransport {
    async fn connect_url(&self, uri: Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let optional_tor_client = self.tor_client.get().map(|c| c.clone());
        let host = extract_host(resolved_data, "SimpleProxyTransport")?;
        let join_handle = tokio::task::spawn(async move {
            let  upgraded = hyper::upgrade::on(req).await?;
            let mut upgraded_io = TokioIo::new(upgraded);
            let addr = format!("{}:443", host);
            debug!("Upgrade success. Connecting to a {}", addr);
            if host.ends_with(".onion") {
                optional_tor_connection(optional_tor_client, upgraded_io, &addr, &uri).await
            } else {
                let mut remote = TcpStream::connect(addr).await?;
                copy_bidirectional(&mut upgraded_io, &mut remote).await?;
                Ok::<(), GenericError>(())
            }
        });

        tokio::task::spawn(async {
            handle_result("Connect tunneling", join_handle).await;
        });

        Ok(Response::new(Empty::new().map_err(|never| match never {}).boxed()))
    }

    async fn get_url(&self, uri: &Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let host = extract_host(resolved_data, "SimpleProxyTransport")?;
        if host.ends_with(".onion") {
            debug!("Fetching onion address: {}", uri);
            let optional_tor_client = self.tor_client.get().map(|c| c.clone());
            optional_tor_request(optional_tor_client, &host, req, uri).await
        } else {
            debug!("Fetching plain address: {}", uri);
            http_get_request(&host, req, &uri).await
        }
    }
}

#[async_trait]
impl ProxyTransport for RedirectProxyTransport {
    async fn connect_url(&self, uri: Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        debug!("Uri is {}", &uri);
        let tls_manager = self.tls_manager.clone();
        let host = extract_host(resolved_data, "RedirectProxyTransport")?;
        let join_handle = tokio::task::spawn(async move {
            let new_uri = replace_host(&uri, &host).map_err(|_| "Failed to replace host".to_owned())?;
            let upgraded = hyper::upgrade::on(req).await?;
            let upgraded_io = TokioIo::new(upgraded);
            debug!("Upgrade success. Making redirect for {} to {}", uri, new_uri);
            let tls_config = tls_manager.get_config(uri.host().unwrap())?;
            let acceptor = TlsAcceptor::from(tls_config);
            let mut stream = acceptor.accept(upgraded_io).await?;
            let fut = async move {
                let mut output = sink();
                let content = format!("HTTP/1.0 301 Moved Permanently\r\n\
                    Location: {}\r\n
                    \r\n", new_uri.clone());
                stream.write_all(content.as_bytes()).await?;
                stream.shutdown().await?;
                copy(&mut stream, &mut output).await
            };
            fut.await?;
            Ok::<(), GenericError>(())
        });

        tokio::task::spawn(async {
            handle_result("Connect tunneling", join_handle).await;
        });

        Ok(Response::new(Empty::new().map_err(|never| match never {}).boxed()))
    }

    async fn get_url(&self, uri: &Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let host = extract_host(resolved_data, "RedirectProxyTransport")?;
        let new_uri = replace_host(uri, &host).map_err(|_| "Failed to replace host".to_owned())?;
        if new_uri.scheme_str().map(|s| s == "https").unwrap_or(false) {
            debug!("Redirecting to: {}", new_uri);
            moved_permanently(&new_uri)
        } else {
            debug!("Fetching address: {}", new_uri);
            let host = new_uri.host().unwrap_or(&host);
            let new_req = replace_request_host(req, &new_uri)?;
            if new_uri.host().map(|h| h.ends_with(".onion")).unwrap_or(false) {
                debug!("Fetching onion address: {}", new_uri);
                let optional_tor_client = self.tor_client.get().map(|c| c.clone());
                optional_tor_request(optional_tor_client, &host, new_req, &new_uri).await
            } else {
                debug!("Fetching plain address: {}", new_uri);
                http_get_request(&host, new_req, &new_uri).await
            }
        }
    }
}

#[async_trait]
impl ProxyTransport for OnchainProxyTransport {
    async fn connect_url(&self, uri: Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let tls_manager = self.tls_manager.clone();
        let (content, content_type) = extract_content(resolved_data, "OnchainProxyTransport")?;
        let join_handle = tokio::task::spawn(async move {
            let upgraded = hyper::upgrade::on(req).await?;
            let upgraded_io = TokioIo::new(upgraded);
            debug!("Upgrade success. Serving onchain site for {}", uri);
            let tls_config = tls_manager.get_config(uri.host().unwrap())?;
            let acceptor = TlsAcceptor::from(tls_config);
            let mut stream = acceptor.accept(upgraded_io).await?;
            let fut = async move {
                let mut output = sink();
                let content = format!("HTTP/1.0 200 ok\r\n\
                    Connection: close\r\n\
                    Content-type: {}\r\n
                    \r\n\
                    {}", content_type, content);
                stream.write_all(content.as_bytes()).await?;
                stream.shutdown().await?;
                copy(&mut stream, &mut output).await
            };
            fut.await?;
            Ok::<(), GenericError>(())
        });

        tokio::task::spawn(async {
            handle_result("Connect tunneling", join_handle).await;
        });

        Ok(Response::new(Empty::new().map_err(|never| match never {}).boxed()))
    }

    async fn get_url(&self, uri: &Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        debug!("Returning html from address: {}", uri);
        let (content, content_type) = extract_content(resolved_data, "OnchainProxyTransport")?;
        let mut builder = Response::builder().status(StatusCode::OK);
        builder = builder.header(header::CONTENT_TYPE, content_type);
        builder.body(Full::new(Bytes::from(content)).map_err(|never| match never {}).boxed()).map_err(|e| Box::new(e) as _)
    }
}

/// Original implementation of TOR proxy.
/// Basically not working solution for HTTPS.
/// Every site backend interprets Host header as they want.
/// It could be server error, redirect or whatever.
/// That's why it's better to redirect to correct target.
/// TODO: implement it in VPN (MITM) style for HTTPS
#[async_trait]
impl ProxyTransport for TorProxyTransport {
    async fn connect_url(&self, uri: Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let optional_tor_client = self.tor_client.get().map(|c| c.clone());
        let host = extract_host(resolved_data, "TorProxyTransport")?;
        let join_handle = tokio::task::spawn(async move {
            let new_uri = replace_host(&uri, &host).map_err(|_| "Failed to replace host".to_owned())?;
            let upgraded = hyper::upgrade::on(req).await?;
            let upgraded_io = TokioIo::new(upgraded);
            let new_host = new_uri.authority().map(|a| a.host()).unwrap_or_else(|| "").to_owned();
            let addr = format!("{}:443", new_host);
            debug!("Upgrade success. Connecting to a {}", addr);
            optional_tor_connection(optional_tor_client, upgraded_io, &addr, &uri).await
        });

        tokio::task::spawn(async {
            handle_result("Connect tunneling", join_handle).await;
        });

        Ok(Response::new(Empty::new().map_err(|never| match never {}).boxed()))
    }

    async fn get_url(&self, uri: &Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let host = extract_host(resolved_data, "TorProxyTransport")?;
        let new_uri = replace_host(uri, &host).map_err(|_| "Failed to replace host".to_owned())?;
        if new_uri.scheme_str().map(|s| s == "https").unwrap_or(false) {
            let redirect_original_uri = replace_scheme(req.uri()).map_err(|_| "Failed to replace schema".to_owned())?;
            debug!("Redirecting to: {}", redirect_original_uri);
            moved_permanently(&redirect_original_uri)
        } else {
            debug!("Fetching onion address: {}", new_uri);
            let new_req = replace_request_host(req, &new_uri)?;
            let optional_tor_client = self.tor_client.get().map(|c| c.clone());
            optional_tor_request(optional_tor_client, &host, new_req, &new_uri).await
        }
    }

}

/// Original implementation of WEB2 proxy (aka plain, web top level, etc).
/// Basically not working solution for HTTPS.
/// During TLS handshake browser send Client hello TCP packet with SNI field (servername).
/// This field is used by server to determine which certificate to use.
/// Server doesn't know about ever-domain and can't serve correct certificate, handshake is aborted with error 40.
/// That's why it's better to redirect to correct target.
/// TODO: implement it in VPN (MITM) style for HTTPS
#[async_trait]
impl ProxyTransport for IpProxyTransport {
    async fn connect_url(&self, uri: Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let host = extract_host(resolved_data, "IpProxyTransport")?;
        let join_handle = tokio::task::spawn(async move {
            let new_uri = replace_host(&uri, &host).map_err(|_| "Failed to replace host".to_owned())?;
            let new_host = new_uri.authority().map(|a| a.host()).unwrap_or_else(|| "").to_owned();
            let addr = format!("{}:443", new_host);
            let upgraded = hyper::upgrade::on(req).await?;
            let mut upgraded_io = TokioIo::new(upgraded);
            debug!("Upgrade success. Connecting to a {}", addr);
            let mut remote = TcpStream::connect(addr).await?;
            copy_bidirectional(&mut upgraded_io, &mut remote).await?;
            Ok::<(), GenericError>(())
        });

        tokio::task::spawn(async {
            handle_result("Connect tunneling", join_handle).await;
        });

        Ok(Response::new(Empty::new().map_err(|never| match never {}).boxed()))
    }

    async fn get_url(&self, uri: &Uri, resolved_data: ResolvedDomainData, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let host = extract_host(resolved_data, "IpProxyTransport")?;
        let new_uri = replace_host(uri, &host).map_err(|_| "Failed to replace host".to_owned())?;
        if new_uri.scheme_str().map(|s| s == "https").unwrap_or(false) {
            let redirect_original_uri = replace_scheme(req.uri()).map_err(|_| "Failed to replace schema".to_owned())?;
            debug!("Redirecting to: {}", redirect_original_uri);
            moved_permanently(&redirect_original_uri)
        } else {
            debug!("Fetching plain address: {}", new_uri);
            let new_req = replace_request_host(req, &new_uri)?;
            http_get_request(&host, new_req, &new_uri).await
        }
    }
}

fn replace_host(uri: &Uri, new_host: &str) -> Result<Uri, ()> {
    let new_host_uri: Uri = new_host.parse().map_err(|_| ())?;
    let scheme = new_host_uri.scheme_str().unwrap_or_else(|| uri.scheme_str().unwrap_or("http"));
    let authority = new_host_uri.authority().map_or_else(|| "".to_string(), |a| a.to_string());
    let path_and_query = uri.path_and_query().map_or_else(|| "".to_string(), |pq| pq.to_string());
    Uri::builder()
        .scheme(scheme)
        .authority(authority)
        .path_and_query(path_and_query)
        .build()
        .map_err(|_| ())
}

fn replace_request_host(request: Request<Incoming>, uri: &Uri) -> Result<Request<Incoming>, GenericError> {
    match uri.host() {
        Some(host) => {
            let (mut parts, body) = request.into_parts();
            parts.uri = uri.clone();
            parts.headers.insert(header::HOST, HeaderValue::from_str(host)?);
            Ok(Request::from_parts(parts, body))
        },
        None => Err(SimpleError::boxed("Malformed remote address"))
    }
}

fn replace_scheme(uri: &Uri) -> Result<Uri, ()> {
    let authority = uri.authority().map_or_else(|| "".to_string(), |a| a.to_string());
    let path_and_query = uri.path_and_query().map_or_else(|| "".to_string(), |pq| pq.to_string());
    Uri::builder()
        .scheme("https")
        .authority(authority)
        .path_and_query(path_and_query)
        .build()
        .map_err(|_| ())
}

fn moved_permanently(new_location: &Uri) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
    Builder::new()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, new_location.to_string())
        .body(Full::new(Bytes::from("Moved Permanently")).map_err(|never| match never {}).boxed())
        .map_err(|e| Box::new(e) as _)
}

fn extract_host(resolved_data: ResolvedDomainData, transport_name: &str) -> Result<String, GenericError> {
    match resolved_data {
        ResolvedDomainData::DomainString(host) => Ok(host),
        _ => Err(SimpleError::boxed(&format!("Wrong resolved data for {}: {}", transport_name, resolved_data)))
    }
}

fn extract_content(resolved_data: ResolvedDomainData, transport_name: &str) -> Result<(String, String), GenericError> {
    match resolved_data {
        ResolvedDomainData::OnchainData(content) => Ok((content, "text/html; charset=utf-8".to_string())),
        ResolvedDomainData::OnchainContractData((content, content_type)) => Ok((content, content_type)),
        _ => Err(SimpleError::boxed(&format!("Wrong resolved data for {}: {}", transport_name, resolved_data)))
    }
}

async fn http_get_request(host: &str, req: Request<Incoming>, uri: &Uri) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
    let port = uri.port_u16().unwrap_or(80);
    let stream = TcpStream::connect((host, port)).await?;
    let (mut request_sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            error!("Connection failed: {:?}", err);
        }
    });
    let resp = request_sender.send_request(req).await?;
    Ok(resp.map(|i| i.boxed()))
}

async fn optional_tor_request(client: Option<Arc<TorClient<PreferredRuntime>>>,
                              host: &str,
                              request: Request<Incoming>,
                              uri: &Uri) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
    if let Some(tor_client) = client {
        let port = uri.port_u16().unwrap_or(80);
        let stream = tor_client.connect((host, port)).await?;
        let (mut request_sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                error!("Connection to TOR failed: {:?}", err);
            }
        });
        let resp = request_sender.send_request(request).await?;
        Ok(resp.map(|i| i.boxed()))
    } else {
        error!("TOR client is not initialized! Could not serve onion site for {}", uri);
        Err(SimpleError::boxed("TOR client is not initialized!"))
    }
}

async fn optional_tor_connection(client: Option<Arc<TorClient<PreferredRuntime>>>, mut upgraded: TokioIo<Upgraded>, addr: &str, uri: &Uri) -> Result<(), GenericError> {
    if let Some(tor_client) = client {
        let mut remote = tor_client.connect(addr).await?;
        copy_bidirectional(&mut upgraded, &mut remote).await?;
        Ok::<(), GenericError>(())
    } else {
        error!("TOR client is not initialized! Could not serve onion site for {}", uri);
        Err(SimpleError::boxed("TOR client is not initialized!"))
    }
}

pub async fn handle_result<T>(title: &str, handle: JoinHandle<Result<T, GenericError>>) {
    match handle.await {
        Ok(Ok(_)) => {
            debug!("{} task completed successfully", title);
        }
        Ok(Err(e)) => {
            error!("{} task returned an error: {}", title, e);
        }
        Err(_) => {
            error!("{} task panicked", title);
        }
    }
}
