use super::schema::{validate_profile, OverlapProfile, ValidationIssue};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const EMBEDDED_OVERLAP: &[(&str, &str)] = &[
    ("rtk", include_str!("../../overlap-profiles/rtk.toml")),
    (
        "headroom",
        include_str!("../../overlap-profiles/headroom.toml"),
    ),
    (
        "caveman",
        include_str!("../../overlap-profiles/caveman.toml"),
    ),
];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignatureEnvelope {
    pub key_id: String,
    pub alg: String,
    pub sha256: String,
    pub signature: String,
}

pub trait SignatureVerifier {
    fn verify(&self, payload: &[u8], env: &SignatureEnvelope) -> bool;
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut k_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let h = Sha256::digest(key);
        k_block[..h.len()].copy_from_slice(&h);
    } else {
        k_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k_block[i];
        opad[i] ^= k_block[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub struct HmacVerifier {
    pub key_id: String,
    pub key: Vec<u8>,
}

impl HmacVerifier {
    pub fn sign(&self, payload: &[u8]) -> SignatureEnvelope {
        let sha256 = sha256_hex(payload);
        let msg = format!("{}\n{}", self.key_id, sha256);
        let signature = hmac_sha256_hex(&self.key, msg.as_bytes());
        SignatureEnvelope {
            key_id: self.key_id.clone(),
            alg: "hmac-sha256".into(),
            sha256,
            signature,
        }
    }
}

impl SignatureVerifier for HmacVerifier {
    fn verify(&self, payload: &[u8], env: &SignatureEnvelope) -> bool {
        if env.alg != "hmac-sha256" || env.key_id != self.key_id {
            return false;
        }
        let actual_sha = sha256_hex(payload);
        if !ct_eq(actual_sha.as_bytes(), env.sha256.as_bytes()) {
            return false;
        }
        let msg = format!("{}\n{}", env.key_id, env.sha256);
        let expected_sig = hmac_sha256_hex(&self.key, msg.as_bytes());
        ct_eq(expected_sig.as_bytes(), env.signature.as_bytes())
    }
}

#[derive(Debug, Default)]
pub struct LoadedProfiles {
    pub profiles: Vec<OverlapProfile>,
    pub warnings: Vec<String>,
}

pub fn load_builtin(today_iso: &str) -> LoadedProfiles {
    let mut out = LoadedProfiles::default();
    for (id, src) in EMBEDDED_OVERLAP {
        match toml::from_str::<OverlapProfile>(src) {
            Ok(p) => {
                for issue in validate_profile(&p, today_iso) {
                    match issue {
                        ValidationIssue::Error(e) => {
                            out.warnings.push(format!("built-in profile error: {e}"))
                        }
                        ValidationIssue::Warning(w) => out.warnings.push(w),
                    }
                }
                out.profiles.push(p);
            }
            Err(e) => {
                out.warnings.push(format!(
                    "failed to parse built-in overlap profile '{id}': {e}"
                ));
            }
        }
    }
    out
}

pub fn load_with_signed_dir(
    dir: &Path,
    verifier: &dyn SignatureVerifier,
    today_iso: &str,
) -> LoadedProfiles {
    let mut loaded = load_builtin(today_iso);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return loaded;
    };
    for ent in entries.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let mut sig_os = path.as_os_str().to_os_string();
        sig_os.push(".sig");
        let sig_path = std::path::PathBuf::from(sig_os);

        let Ok(payload) = std::fs::read(&path) else {
            loaded
                .warnings
                .push(format!("cannot read overlap profile {}", path.display()));
            continue;
        };
        let Ok(sig_bytes) = std::fs::read(&sig_path) else {
            loaded.warnings.push(format!(
                "rejected unsigned overlap profile {} (missing .sig)",
                path.display()
            ));
            continue;
        };
        let Ok(env) = serde_json::from_slice::<SignatureEnvelope>(&sig_bytes) else {
            loaded.warnings.push(format!(
                "rejected overlap profile {} (invalid signature envelope)",
                path.display()
            ));
            continue;
        };
        if !verifier.verify(&payload, &env) {
            loaded.warnings.push(format!(
                "rejected tampered overlap profile {} (signature mismatch)",
                path.display()
            ));
            continue;
        }
        let Ok(text) = std::str::from_utf8(&payload) else {
            loaded.warnings.push(format!(
                "rejected non-UTF-8 overlap profile {}",
                path.display()
            ));
            continue;
        };
        match toml::from_str::<OverlapProfile>(text) {
            Ok(p) => {
                let mut hard_err = false;
                for issue in validate_profile(&p, today_iso) {
                    match issue {
                        ValidationIssue::Error(e) => {
                            loaded
                                .warnings
                                .push(format!("rejected invalid profile: {e}"));
                            hard_err = true;
                        }
                        ValidationIssue::Warning(w) => loaded.warnings.push(w),
                    }
                }
                if !hard_err {
                    if let Some(pos) = loaded.profiles.iter().position(|x| x.id == p.id) {
                        loaded.profiles[pos] = p;
                    } else {
                        loaded.profiles.push(p);
                    }
                }
            }
            Err(e) => {
                loaded.warnings.push(format!(
                    "ignored malformed overlap profile {}: {e}",
                    path.display()
                ));
            }
        }
    }
    loaded
}
