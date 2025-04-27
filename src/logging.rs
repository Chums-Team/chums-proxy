use anyhow::{anyhow, Result};
use std::error::Error;
use std::fmt::Display;
use std::sync::Once;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use tracing::Level;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use tracing_log::LogTracer;

#[cfg(any(target_os = "android", target_os = "ios"))]
use log::LevelFilter;

#[cfg(target_os = "android")]
use android_logger::Config;

#[cfg(target_os = "ios")]
use oslog::OsLogger;

pub type GenericError = Box<dyn Error + Send + Sync>;

#[derive(Debug)]
pub struct SimpleError {
    message: String,
}

impl SimpleError {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }

    pub fn boxed(message: &str) -> Box<dyn Error + Send + Sync> {
        Box::new(Self::new(message))
    }
}

impl Display for SimpleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for SimpleError {}

static INIT: Once = Once::new();

pub fn init(log_level: &str) -> Result<()> {

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let log_level = match log_level {
            "error" => Ok(Level::ERROR),
            "warn" => Ok(Level::WARN),
            "info" => Ok(Level::INFO),
            "debug" => Ok(Level::DEBUG),
            "trace" => Ok(Level::TRACE),
            _ => Err(anyhow!("Invalid log level"))
        }?;
        INIT.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(log_level)
                .finish();
            tracing::subscriber::set_global_default(subscriber).unwrap();
            LogTracer::init().unwrap();
        });
        Ok(())
    }

    #[cfg(target_os = "android")]
    {
        let log_level = match log_level {
            "error" => Ok(LevelFilter::Error),
            "warn" => Ok(LevelFilter::Warn),
            "info" => Ok(LevelFilter::Info),
            "debug" => Ok(LevelFilter::Debug),
            "trace" => Ok(LevelFilter::Trace),
            _ => Err(anyhow!("Invalid log level"))
        }?;
        INIT.call_once(|| {
            android_logger::init_once(
                Config::default().with_max_level(log_level),
            );
        });
        Ok(())
    }

    #[cfg(target_os = "ios")]
    {
        let log_level = match log_level {
            "error" => Ok(LevelFilter::Error),
            "warn" => Ok(LevelFilter::Warn),
            "info" => Ok(LevelFilter::Info),
            "debug" => Ok(LevelFilter::Debug),
            "trace" => Ok(LevelFilter::Trace),
            _ => Err(anyhow!("Invalid log level"))
        }?;
        INIT.call_once(|| {
            OsLogger::new("evername")
                .level_filter(log_level)
                .init().unwrap()
        });
        Ok(())
    }
}
