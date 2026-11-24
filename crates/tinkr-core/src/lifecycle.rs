use std::{
    net::{SocketAddr, TcpStream},
    path::Path,
    time::Duration,
};

pub fn systemd_unit(proxy_exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Tinkr loopback token-optimization proxy\n\
         After=network.target\n\n\
         [Service]\n\
         ExecStart={}\n\
         Restart=on-failure\n\
         RestartSec=2\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        proxy_exe.display()
    )
}

pub fn launchd_plist(proxy_exe: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n  \
           <key>Label</key><string>dev.tinkr.proxy</string>\n  \
           <key>ProgramArguments</key><array><string>{}</string></array>\n  \
           <key>RunAtLoad</key><true/>\n  \
           <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n\
         </dict>\n</plist>\n",
        proxy_exe.display()
    )
}

pub fn windows_schtasks_args(proxy_exe: &Path) -> Vec<String> {
    vec![
        "/Create".into(),
        "/SC".into(),
        "ONLOGON".into(),
        "/TN".into(),
        "TinkrProxy".into(),
        "/TR".into(),
        format!("\"{}\"", proxy_exe.display()),
        "/F".into(),
    ]
}

pub fn is_proxy_alive(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok()
}

pub fn resolve_endpoint(port: Option<u16>, route: &str, direct_upstream: &str) -> String {
    if let Some(p) = port {
        if is_proxy_alive(p) {
            return format!("http://127.0.0.1:{p}/{route}");
        }
    }
    direct_upstream.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_definitions_and_failopen_fallback() {
        let exe = Path::new("/usr/local/bin/tinkr-proxy");
        assert!(systemd_unit(exe).contains("ExecStart=/usr/local/bin/tinkr-proxy"));
        assert!(launchd_plist(exe).contains("dev.tinkr.proxy"));
        assert!(windows_schtasks_args(exe).contains(&"TinkrProxy".to_string()));
        assert_eq!(
            resolve_endpoint(Some(1), "anthropic", "https://api.anthropic.com"),
            "https://api.anthropic.com"
        );
        assert_eq!(
            resolve_endpoint(None, "openai", "https://api.openai.com"),
            "https://api.openai.com"
        );
    }
}
