use crate::transport::handle_result;
use http::Method;
use http_body_util::combinators::BoxBody;
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{http, Request, Response};
use hyper_util::rt::TokioIo;
use libc::c_char;
use log::{debug, error, info};
use logging::GenericError;
use proxy::ProxyState;
use std::ffi::CStr;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tokio::sync::oneshot::{Receiver, Sender};

mod logging;
mod proxy;
mod transport;
mod tls;

#[repr(C)]
pub struct ServerHandle {
    tokio_runtime: *mut Runtime,
    shutdown_sender: *mut Sender<()>,
}

#[no_mangle]
pub extern "C" fn start_server_ffi(host: *const c_char,
                                   port: u16,
                                   log_level: *const c_char,
                                   state_dir: *const c_char,
                                   cache_dir: *const c_char) -> *mut ServerHandle {

    let (shutdown_sender, shutdown_rx) = oneshot::channel::<()>();

    match parse_ffi_params(host, log_level, state_dir, cache_dir) {

        Ok((host, log_level, state_dir, cache_dir, rt)) => {

            if let Err(e) = logging::init(log_level.as_str()) {
                panic!("Failed to start server: wrong log level: {}", e);
            }

            info!("Starting server ...");
            let join_handle = rt.spawn(async move {
                run_server(SocketAddr::new(host, port), shutdown_rx, Some(state_dir), Some(cache_dir)).await
            });
            rt.spawn(async {
                handle_result("Proxy server", join_handle).await;
            });

            Box::into_raw(
                Box::new(
                    ServerHandle {
                        tokio_runtime: Box::into_raw(Box::new(rt)),
                        shutdown_sender: Box::into_raw(Box::new(shutdown_sender))
                    }
                )
            )
        },
        Err(e) => {
            logging::init("trace").unwrap();
            error!("Failed to start server: {}", e);
            panic!("Failed to start server: {}", e)
        }
    }
}

fn parse_ffi_params(host: *const c_char,
                    log_level: *const c_char,
                    state_dir: *const c_char,
                    cache_dir: *const c_char) -> Result<(IpAddr, String, String, String, Runtime), GenericError> {

    let host = unsafe { CStr::from_ptr(host) };
    let host = host.to_str()?;
    let host = IpAddr::from_str(host)?;

    let log_level = unsafe { CStr::from_ptr(log_level) };
    let log_level = log_level.to_str()?;

    let state_dir = unsafe { CStr::from_ptr(state_dir) };
    let state_dir = state_dir.to_str()?;

    let cache_dir = unsafe { CStr::from_ptr(cache_dir) };
    let cache_dir = cache_dir.to_str()?;

    let rt = Runtime::new()?;

    Ok((host, log_level.to_owned(), state_dir.to_owned(), cache_dir.to_owned(), rt))
}

#[no_mangle]
pub extern "C" fn stop_server_ffi(handle_ptr: *mut ServerHandle) {
    let handle = unsafe { Box::from_raw(handle_ptr) };
    let tokio_runtime = unsafe { Box::from_raw(handle.tokio_runtime) };
    let shutdown_sender = unsafe { Box::from_raw(handle.shutdown_sender) };
    let _ = shutdown_sender.send(());
    tokio_runtime.shutdown_timeout(Duration::from_secs(5));
}

pub async fn run_server(addr: SocketAddr, 
                        shutdown_rx: Receiver<()>, 
                        state_dir: Option<String>, 
                        cache_dir: Option<String>) -> Result<(), GenericError> {
    let state = Arc::new(ProxyState::new(state_dir, cache_dir).await?);
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on http://{}", addr);
    let http = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let mut signal = std::pin::pin!(async {
        shutdown_rx.await.ok();
    });
    
    loop {
        tokio::select! {
            Ok((stream, _addr)) = listener.accept() => {
                let state = Arc::clone(&state);
                let io = TokioIo::new(stream);
                let conn = http.serve_connection_with_upgrades(io, service_fn(move |req|
                    serve_proxy_req(Arc::clone(&state), req)
                ));
                let conn = graceful.watch(conn.into_owned());
                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        error!("Error serving connection: {:.?}", e);
                    }
                });
            },
            _ = &mut signal => {
                info!("Shutting down server");
                break;
            }
        }
    }

    tokio::select! {
        _ = graceful.shutdown() => {
            info!("all connections gracefully closed");
        },
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
            error!("timed out wait for all connections to close");
        }
    }
    Ok(())
}

async fn serve_proxy_req(
    proxy_state: Arc<ProxyState>,
    req: Request<Incoming>
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
    if req.method() == Method::CONNECT {
        debug!("Serving CONNECT");
        proxy_state.connect_url(req).await.map_err(|e| {
            error!("Failed to serve CONNECT: {}", e);
            e
        })
    } else {
        debug!("Serving GET");
        proxy_state.fetch_url(req).await
    }
}
