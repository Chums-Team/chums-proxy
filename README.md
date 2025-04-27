# Chums proxy

A Rust **HTTP / HTTPS proxy** capable of handling Web3-domain and landing user to target website.
Target website could be either Web2-domain, IPFS, Tor Onion or onchain site on everscale network 
(with [special contracts](https://github.com/Chums-Team/everscale-onchain-site-contract)).

It can be built as a standalone binary or used as a library.
Also, you could use it in almost any language via the provided FFI interface.
For example, Dart/Flutter adoption already exists in https://github.com/Chums-Team/flutter-chums-proxy.

It is based on [web3-resolver](https://github.com/Chums-Team/web3-resolver) library. 
So it is capable of resolving the following domains:
* Evername domains (.ever) could be resolved to:
   - Tor (query key = 1001)
   - IPFS (query key = 1002)
   - Web2-domain address (query key = 1003)
   - Onchain site (content stored directly in the domain NFT, size is *very* limited) (query key = 1004)
   - OnchainContract (content stored in the separate [eversite contract](https://github.com/Chums-Team/everscale-onchain-site-contract), size is limited) (query key = 1005)
* Unstoppable Domains (.crypto, .x, .blockchain, .nft, .wallet, etc.) could be resolved to plain url
* Simple web2 domains when non-web3 address is provided (domain ending is not an .ever or Unstoppable Domains TLD, e.g. .com, .net, etc.)

## Build
System's arch build: 
```shell
cargo build --release
```
For cross-build you have to install `cross` tool. Cross tool requires container engine, i.e. `docker` to be installed. 
```shell
cargo install cross
```
Check that you have appropriate toolchains installed:
```shell
rustup show
```
If you don't have say `aarch64-linux-android` toolchain, install it:
```shell
rustup target add aarch64-linux-android
```
Then you can build for Android:
```shell
cross build --target aarch64-linux-android --lib --release
```

After building you will get both library and binary under `target/{toolchain_name}/release` folder.

## Run standalone proxy
After compilation:
```shell
./target/release/chums-proxy -h
Rust HTTP(S) proxy for Web3 domains

Usage: chums-proxy [OPTIONS]

Options:
  -i, --ip <IP>          The IP address to bind on [default: 127.0.0.1]
  -p, --port <PORT>      The port to bind on [default: 3000]
  -l, --log <LOG_LEVEL>  The log level to use [default: info]
  -h, --help             Print help
  -V, --version          Print version
```
This will start the proxy on the configured host and port.
```shell
# Run the proxy with defaults
./target/release/chums-proxy
# Or customize it
./target/release/chums-proxy -i 0.0.0.0 -p 8080 -l debug
```

After starting the proxy, you can go to your browser settings and specify the proxy server address and port. 
Then you can open any Web3 domain (e.g., http://dns-proxy.chums.ever), and the proxy will resolve it and lead you to the content.

## Use as a library
### In Rust
```rust
use chums_proxy::run_server;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use tokio::sync::oneshot;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = SocketAddr::new(IpAddr::from_str("127.0.0.1")?, 8080);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    // Start the server
    let server = tokio::spawn(run_server(addr, shutdown_rx, None, None));

    // To stop the server
    // shutdown_tx.send(()).unwrap();

    server.await?;
    Ok(())
}
```

### Through FFI interface
```c
#include <stdio.h>
#include <stdint.h>

typedef struct ServerHandle ServerHandle;

extern ServerHandle* start_server_ffi(const char* host,
                                      uint16_t port,
                                      const char* log_level,
                                      const char* state_dir,
                                      const char* cache_dir);

extern void stop_server_ffi(ServerHandle* handle);

int main() {
    ServerHandle* handle = start_server_ffi("127.0.0.1", 8080, "info", "/tmp/state", "/tmp/cache");

    printf("Proxy server started. Press Enter to stop...\n");
    getchar();

    stop_server_ffi(handle);
    printf("Proxy server stopped\n");

    return 0;
}
```

### Configuration
The proxy server supports the following parameters:
- `host`: IP address to bind the server
- `port`: Port to listen for connections
- `log_level`: Logging level (trace, debug, info, warn, error)
- `state_dir`: Directory for storing state files for Tor connections (could be empty)
- `cache_dir`: Directory for storing cache for Tor connections (could be empty)

## Limitations!
By now Chums-proxy has several limitations and restrictions. 
It's main goal is to be a simple way of landing and leading user to the target website/content under the Web3-domain.
It doesn't do magic (at least for now) and doesn't try to be a full-featured proxy server.

### Request types on HTTP
Chums-proxy could properly handle any HTTPS traffic. 
However, by now it is capable of handling **ONLY HTTP GET requests**.

### Dealing with HTTPS
1\. Chums-proxy does not act as man-in-the-middle (MITM) proxy. 
It serves HTTP CONNECT request to a proper target.
Than it just forwards the encrypted traffic to the target server and back without any capabilities to decrypt it.

2\. You cannot get an HTTPS website content using Chums-proxy with a web3-domain (e.g., https://dns-proxy.chums.ever) in your browser address bar. 

Why?

If you enter the URL with HTTPS protocol and web3-domain, your browser will send CONNECT request to the proxy server.
Chums-proxy will transfer traffic to the target server.
During TLS handshake, the target server will send its certificate to the browser.
Your browser won't be able to validate the certificate because of the wrong domain name.

That's why Chums-proxy offers [redirection](#redirection) to the target website in several cases with HTTPS

### Onchain sites & HTTPS
Onchain sites are not hosted anywhere, both direct in the domain NFT and in the separate contract.
But it is desirable to have HTTPS support for them.
To achieve this Chums-proxy generates on the fly a self-signed certificate for every domain name.
That's why **your browser will show a warning about the certificate**.

### Redirection
Based on above limitations, Chums-proxy offers redirection to the target website in the following cases:
| Domain type        | Address tag      | Protocol in address bar | Target protocol | Redirect |
| ------------------ | ---------------- | ----------------------- | --------------- | -------- |
| Evername           | Tor              | http                    | http            | No\*     |
| Evername           | Tor              | http                    | https           | Yes      |
| Evername           | Tor              | http/https              | https           | Yes      |
| Evername           | IPFS             | http/https              | always https    | Yes      |
| Evername           | Web2             | http                    | http            | No       |
| Evername           | Web2             | http                    | https           | Yes      |
| Evername           | Web2             | https                   | http/https      | Yes      |
| Evername           | Onchain          | http/https              | -               | No       |
| Evername           | Onchain Contract | http/https              | -               | No       |
| Unstoppable Domain | -                | http                    | http            | No       |
| Unstoppable Domain | -                | http                    | https           | Yes       |
| Unstoppable Domain | -                | http/https              | https           | Yes      |

\* some Tor-sites force redirect to right onion-site when "wrong" domain is used. 

## License

This project is licensed under the Apache 2.0 License. See the [LICENSE](LICENSE) file for details.
