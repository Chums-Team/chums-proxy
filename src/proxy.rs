use std::collections::HashMap;
use std::sync::Arc;

use crate::logging::{GenericError, SimpleError};
use crate::tls::TlsManager;
use crate::transport::{IpProxyTransport, OnchainProxyTransport, ProxyTransport, RedirectProxyTransport, SimpleProxyTransport, TorProxyTransport};
use arti_client::config::CfgPath;
use arti_client::{config::TorClientConfigBuilder, TorClient};
use http::uri::Uri;
use http_body_util::combinators::BoxBody;
use hyper::body::{Bytes, Incoming};
use hyper::{http, Request, Response};
use log::{debug, error, info};
use tokio::sync::OnceCell;
use tor_config;
use tor_rtcompat::PreferredRuntime;
use web3_resolver::models::{AddressTag, ResolvedDomainData};
use web3_resolver::Web3DomainResolver;

const EVERSCALE_RPC_ENDPOINT: &str = "https://jrpc.everwallet.net/rpc";
const CHUMS_UD_PROXY_URL: &str = "https://ud.chums.chat/";

pub struct ProxyState {
    transports: HashMap<AddressTag, Box<dyn ProxyTransport + Send + Sync>>,
    domain_resolver: Arc<Web3DomainResolver>,
}

impl ProxyState {

    pub async fn new(state_dir: Option<String>, cache_dir: Option<String>) -> Result<ProxyState, GenericError> {
        info!("Initializing proxy state...");
        let tor_cell = Arc::new(OnceCell::new());
        let tor_cell_clone = tor_cell.clone();
        let state_dir_clone = state_dir.clone();
        let cache_dir_clone = cache_dir.clone();
        tokio::task::spawn(async move {
            debug!("Lazy initializing TOR client ...");
            let tor_client = make_tor_client(state_dir_clone, cache_dir_clone).await?;
            tor_cell_clone.set(Arc::new(tor_client)).map_err(|_| {
                error!("Could not set initialized TOR client");
                "Could not set initialized TOR client".to_owned()
            })?;
            debug!("TOR client created");
            Ok::<(), GenericError>(())
        });

        debug!("Creating TLS manager");
        let tls_manager = TlsManager::new();
        let tls_manager = Arc::new(tls_manager);
        debug!("TLS config created");

        let domain_resolver = Arc::new(Web3DomainResolver::builder()
            .use_cache(true)
            .cache_ttl_seconds(5 * 30)
            .with_eversacale_endpoint(EVERSCALE_RPC_ENDPOINT)
            .with_unstoppable_domain_base_url(CHUMS_UD_PROXY_URL)
            .build().await?);

        debug!("Initializing transports");
        //let tor_transport = TorProxyTransport::new(tor_cell.clone());
        //let ip_transport = IpProxyTransport::new();
        let onchain_transport = OnchainProxyTransport::new(tls_manager.clone());
        let simple_transport = SimpleProxyTransport::new(tor_cell.clone());
        let redirect_transport = RedirectProxyTransport::new(tls_manager.clone(), tor_cell.clone());
        let mut transports: HashMap<AddressTag, Box<dyn ProxyTransport + Send + Sync>> = HashMap::new();
        transports.insert(AddressTag::Tor, Box::new(redirect_transport.clone()));
        transports.insert(AddressTag::Ipfs, Box::new(redirect_transport.clone()));
        transports.insert(AddressTag::Web2, Box::new(redirect_transport.clone()));
        transports.insert(AddressTag::Onchain, Box::new(onchain_transport.clone()));
        transports.insert(AddressTag::OnchainContract, Box::new(onchain_transport));
        transports.insert(AddressTag::NonWeb3, Box::new(simple_transport));
        transports.insert(AddressTag::UnstoppableDomain, Box::new(redirect_transport));
        debug!("Transports initialized");

        debug!("Proxy state successfully initialized");
        Ok(ProxyState {
            transports,
            domain_resolver
        })
    }

    pub async fn connect_url(&self, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let uri = req.uri().clone();
        let (resolved_data, address_tag) = self.resolve(&uri).await?;
        let transport = self.get_transport(&address_tag)?;
        let result = transport.connect_url(uri, resolved_data, req).await;
        result
    }

    pub async fn fetch_url(&self, req: Request<Incoming>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, GenericError> {
        let uri = req.uri().clone();
        let (resolved_data, address_tag) = self.resolve(&uri).await?;
        let transport = self.get_transport(&address_tag)?;
        let result = transport.get_url(&uri, resolved_data, req).await;
        result
    }

    async fn resolve(&self, uri: &Uri) -> Result<(ResolvedDomainData, AddressTag), String> {
        match uri.host() {
            Some(host) => {
                self.domain_resolver.resolve(host).await.map_err(|e| {
                    error!("Failed to resolve domain {}: {}", host, e);
                    format!("Failed to resolve domain {}: {}", host, e)
                })
            },
            None => Err(format!("Malformed remote address: {}", uri))
        }
    }

    fn get_transport(&self, tag: &AddressTag) -> Result<&Box<dyn ProxyTransport + Send + Sync>, GenericError> {
        self.transports.get(tag).ok_or(SimpleError::boxed(&format!("Transport not found for tag {}", tag)))
    }

}

async fn make_tor_client(state_dir: Option<String>, cache_dir: Option<String>) -> Result<TorClient<PreferredRuntime>, GenericError> {
    debug!("Setting up a TOR client ...");
    let mut config_builder = TorClientConfigBuilder::default();
    match state_dir {
        Some(state_dir) => {
            let path = CfgPath::new_literal(state_dir);
            config_builder.storage().state_dir(path);
        },
        None => ()
    };
    match cache_dir {
        Some(cache_dir) => {
            let path = CfgPath::new_literal(cache_dir);
            config_builder.storage().cache_dir(path);
        },
        None => ()
    };
    let config = config_builder.build()?;

    debug!("Created default config");
    let runtime = PreferredRuntime::current()
        .expect("TorClient could not get an asynchronous runtime; are you running in the right context?");
    debug!("Obtained default runtime: {:?}", runtime);
    let tor_client_builder = TorClient::with_runtime(runtime).config(config);
    debug!("Created TOR client builder");
    let mut tor_client = tor_client_builder.create_unbootstrapped()?;
    debug!("Created TOR client, bootstrapping ...");
    let _ = tor_client.bootstrap().await?;
    debug!("TOR client bootstrapped successfully");
    let mut prefs = arti_client::StreamPrefs::default();
    prefs.connect_to_onion_services(tor_config::BoolOrAuto::Explicit(true));
    tor_client.set_stream_prefs(prefs);
    debug!("Updated TOR client preferences");
    Ok(tor_client)
}
