use tinkr_core::platform::{self, Native};
use tinkr_proxy::{spawn_proxy, write_state, ProxyConfig, ProxyState};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("tinkr-proxy {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let port: u16 = args
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(0);

    let paths = platform::paths(&Native).ok();
    let cfg = ProxyConfig {
        paths: paths.clone(),
        ..Default::default()
    };
    let routes = cfg.routes.clone();

    match spawn_proxy(port, cfg).await {
        Ok((addr, _handle)) => {
            if let Some(p) = &paths {
                let state = ProxyState {
                    pid: std::process::id(),
                    port: addr.port(),
                    routes,
                };
                let _ = write_state(&p.data.join("proxy.json"), &state);
            }
            println!("tinkr-proxy listening on http://{addr}");
            let _ = tokio::signal::ctrl_c().await;
            if let Some(p) = &paths {
                let _ = std::fs::remove_file(p.data.join("proxy.json"));
            }
        }
        Err(e) => {
            eprintln!("tinkr-proxy: failed to bind: {e}");
            std::process::exit(1);
        }
    }
}
