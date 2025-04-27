use anyhow::{Context, Result};
use chums_proxy::run_server;
use clap::{arg, command, value_parser};
use std::net::{IpAddr, SocketAddr};
use tokio::sync::oneshot;

mod logging;

#[tokio::main]
async fn main() -> Result<(), logging::GenericError> {

    let (bind_to, log_level) = parse_params()?;
    logging::init(log_level.as_str())?;
    let (_tx, rx) = oneshot::channel::<()>();
    run_server(bind_to, rx, None, None).await?;
    Ok(())
}

fn parse_params() -> Result<(SocketAddr, String)> {

    let matches = command!("Chums proxy")
        .arg(
            arg!(
                -i --ip <IP> "The IP address to bind on"
            )
            .required(false)
            .default_value("127.0.0.1")
            .value_parser(value_parser!(IpAddr))
        )
        .arg(
            arg!(
                -p --port <PORT> "The port to bind on"
            )
            .required(false)
            .default_value("3000")
            .value_parser(value_parser!(u16))
        )
        .arg(
            arg!(
                -l --log <LOG_LEVEL> "The log level to use"
            )
            .required(false)
            .default_value("info")
        )
        .get_matches();

    let ip: IpAddr = matches.get_one::<IpAddr>("ip").cloned().context("Missed IP arg")?;
    let port: u16 = matches.get_one::<u16>("port").cloned().context("Missed port arg")?;
    let log_level = matches.get_one::<String>("log").context("Missed log level arg")?;

    Ok((SocketAddr::new(ip, port), log_level.to_string()))
}
