use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteKind {
    Wsl { distro: String },
    DevContainer,
    Codespaces,
    Docker,
    Ssh,
}

impl RemoteKind {
    pub fn label(&self) -> String {
        match self {
            RemoteKind::Wsl { distro } => format!("WSL ({distro})"),
            RemoteKind::DevContainer => "Dev Container".into(),
            RemoteKind::Codespaces => "GitHub Codespaces".into(),
            RemoteKind::Docker => "Docker container".into(),
            RemoteKind::Ssh => "SSH remote session".into(),
        }
    }
}

pub fn detect_with(
    env: &dyn Fn(&str) -> Option<String>,
    dockerenv_exists: bool,
) -> Vec<RemoteKind> {
    let mut out = Vec::new();
    if let Some(d) = env("WSL_DISTRO_NAME").filter(|s| !s.is_empty()) {
        out.push(RemoteKind::Wsl { distro: d });
    }
    if env("CODESPACES").as_deref() == Some("true") {
        out.push(RemoteKind::Codespaces);
    }
    if env("REMOTE_CONTAINERS").as_deref() == Some("true") || env("DEVCONTAINER").is_some() {
        out.push(RemoteKind::DevContainer);
    }
    if dockerenv_exists {
        out.push(RemoteKind::Docker);
    }
    if env("SSH_CONNECTION").is_some() || env("SSH_CLIENT").is_some() {
        out.push(RemoteKind::Ssh);
    }
    out
}

pub fn detect_current() -> Vec<RemoteKind> {
    detect_with(
        &|k| std::env::var(k).ok(),
        Path::new("/.dockerenv").exists(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_wsl_codespaces_docker_ssh() {
        let env = |k: &str| match k {
            "WSL_DISTRO_NAME" => Some("Ubuntu".into()),
            "CODESPACES" => Some("true".into()),
            "SSH_CONNECTION" => Some("1.2.3.4 22".into()),
            _ => None,
        };
        let found = detect_with(&env, true);
        assert_eq!(found.len(), 4);
        assert!(found.contains(&RemoteKind::Wsl {
            distro: "Ubuntu".into()
        }));
        assert!(found.contains(&RemoteKind::Codespaces));
        assert!(found.contains(&RemoteKind::Docker));
        assert!(found.contains(&RemoteKind::Ssh));
    }
}
