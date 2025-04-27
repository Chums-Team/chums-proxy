use log::debug;
use std::io;
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;

use mini_moka::sync::Cache;
use rcgen::{generate_simple_self_signed, Certificate, CertifiedKey, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use rustls_pemfile::{certs, pkcs8_private_keys};

use crate::logging::{GenericError, SimpleError};

pub struct TlsManager {
    configs: Cache<String, Arc<ServerConfig>>
}

impl TlsManager {
    pub fn new() -> Self {
        let cache = Cache::builder().time_to_live(Duration::from_secs(60 * 60)).build();
        Self {
            configs: cache
        }
    }

    pub fn get_config(&self, domain: &str) -> Result<Arc<ServerConfig>, GenericError> {
        match self.configs.get(&domain.to_string()) {
            Some(config) => Ok(config.clone()),
            None => {
                debug!("Creating TLS certificates and config for domain {}", domain);
                let certificate = make_cert(domain)?;
                let cert = load_cert(&certificate.cert)?;
                let key = load_key(&certificate.key_pair)?;
                let config = ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(cert, key)
                    .map_err(|e| format!("Failed to create TLS config for domain {}: {}", domain, e))?;
                let config = Arc::new(config);
                self.configs.insert(domain.to_string(), config.clone());
                Ok(config)
            }
        }
    }

}

fn make_cert(domain: &str) -> Result<CertifiedKey, GenericError> {
    generate_simple_self_signed(vec![domain.to_string()])
        .map_err(|e| SimpleError::boxed(&e.to_string()))
}

fn load_cert(cert: &Certificate) -> Result<Vec<CertificateDer<'static>>, io::Error> {
    certs(&mut BufReader::new(cert.pem().as_bytes())).collect()
}

fn load_key(key_pair: &KeyPair) -> Result<PrivateKeyDer<'static>, GenericError> {
    let serialized = key_pair.serialize_pem();
    let key = pkcs8_private_keys(&mut BufReader::new(serialized.as_bytes()))
        .next()
        .unwrap()
        .map(Into::into);
    key.map_err(|e| Box::new(e) as _)
}