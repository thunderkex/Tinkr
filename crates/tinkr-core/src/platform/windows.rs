use super::{
    envpersist::{EnvStore, EnvValue},
    winpath, Paths, Platform,
};
use crate::{io_err, Result};
use std::path::{Path, PathBuf};
use winreg::{enums::*, RegKey, RegValue};

#[derive(Debug, Default, Clone, Copy)]
pub struct Native;

pub struct RegistryEnv;

const ENV_KEY: &str = "Environment";

fn reg_err(e: std::io::Error) -> crate::Error {
    io_err(r"HKCU\Environment")(e)
}

fn decode(v: &RegValue) -> Option<EnvValue> {
    let expand = match v.vtype {
        REG_SZ => false,
        REG_EXPAND_SZ => true,
        _ => return None,
    };
    let words: Vec<u16> = v
        .bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let end = words.iter().position(|&w| w == 0).unwrap_or(words.len());
    Some(EnvValue {
        value: String::from_utf16_lossy(&words[..end]),
        expand,
    })
}

fn encode(v: &EnvValue) -> RegValue {
    let mut w: Vec<u16> = v.value.encode_utf16().collect();
    w.push(0);
    let bytes: Vec<u8> = w.iter().flat_map(|u| u.to_le_bytes()).collect();
    RegValue {
        bytes,
        vtype: if v.expand { REG_EXPAND_SZ } else { REG_SZ },
    }
}

fn broadcast() {
    use windows_sys::Win32::{
        Foundation::FreeLibrary,
        System::LibraryLoader::{GetProcAddress, LoadLibraryA},
    };
    type SendMsgTimeoutFn = unsafe extern "system" fn(
        hwnd: isize,
        msg: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        timeout: u32,
        result: *mut usize,
    ) -> isize;
    const HWND_BROADCAST: isize = 0xffff;
    const WM_SETTINGCHANGE: u32 = 0x001A;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;

    let param: Vec<u16> = "Environment\0".encode_utf16().collect();
    let mut result = 0usize;
    unsafe {
        let hmod = LoadLibraryA(c"user32.dll".as_ptr() as *const u8);
        if !hmod.is_null() {
            if let Some(raw_fn) = GetProcAddress(hmod, c"SendMessageTimeoutW".as_ptr() as *const u8)
            {
                let send_msg: SendMsgTimeoutFn = std::mem::transmute(raw_fn);
                send_msg(
                    HWND_BROADCAST,
                    WM_SETTINGCHANGE,
                    0,
                    param.as_ptr() as isize,
                    SMTO_ABORTIFHUNG,
                    2000,
                    &mut result,
                );
            }
            FreeLibrary(hmod);
        }
    }
}

impl EnvStore for RegistryEnv {
    fn name(&self) -> &'static str {
        "windows-registry"
    }
    fn get(&self, name: &str) -> Result<Option<EnvValue>> {
        let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(ENV_KEY, KEY_READ)
        {
            Ok(k) => k,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(reg_err(e)),
        };
        match key.get_raw_value(name) {
            Ok(v) => Ok(decode(&v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(reg_err(e)),
        }
    }
    fn set(&self, name: &str, value: &EnvValue) -> Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey_with_flags(ENV_KEY, KEY_SET_VALUE)
            .map_err(reg_err)?;
        key.set_raw_value(name, &encode(value)).map_err(reg_err)?;
        broadcast();
        Ok(())
    }
    fn unset(&self, name: &str) -> Result<()> {
        let key = match RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(ENV_KEY, KEY_SET_VALUE)
        {
            Ok(k) => k,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(reg_err(e)),
        };
        match key.delete_value(name) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(reg_err(e)),
        }
        broadcast();
        Ok(())
    }
}

impl Platform for Native {
    fn name(&self) -> &'static str {
        "windows"
    }
    fn set_private(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
    fn set_executable(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
    fn env_store(&self, _home: &Path, _paths: &Paths) -> Box<dyn EnvStore> {
        Box::new(RegistryEnv)
    }
    fn extended_path(&self, path: &Path) -> PathBuf {
        PathBuf::from(winpath::windows_extended(&path.to_string_lossy()))
    }
    fn unicode_capable(&self, env: &dyn Fn(&str) -> Option<String>) -> bool {
        ["WT_SESSION", "TERM_PROGRAM", "ConEmuANSI", "ANSICON"]
            .iter()
            .any(|k| env(k).map(|v| !v.is_empty()).unwrap_or(false))
            || env("TERM").map(|t| t.contains("xterm")).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_roundtrip_types_and_cleanup() {
        let s = RegistryEnv;
        let n1 = format!("TINKR_TEST_PLAIN_{}", std::process::id());
        let n2 = format!("TINKR_TEST_EXPAND_{}", std::process::id());
        assert!(s.get(&n1).unwrap().is_none());
        s.set(&n1, &EnvValue::plain("http://127.0.0.1:9 \u{fc}"))
            .unwrap();
        s.set(
            &n2,
            &EnvValue {
                value: "%USERPROFILE%\\x".into(),
                expand: true,
            },
        )
        .unwrap();
        assert_eq!(
            s.get(&n1).unwrap().unwrap(),
            EnvValue::plain("http://127.0.0.1:9 \u{fc}")
        );
        let v2 = s.get(&n2).unwrap().unwrap();
        assert!(v2.expand && v2.value == "%USERPROFILE%\\x");
        s.unset(&n1).unwrap();
        s.unset(&n2).unwrap();
        s.unset(&n2).unwrap();
        assert!(s.get(&n1).unwrap().is_none() && s.get(&n2).unwrap().is_none());
    }

    #[test]
    fn manifest_undo_with_real_registry() {
        use crate::manifest::ChangeSet;
        let d = tempfile::tempdir().unwrap();
        let mut cs = ChangeSet::open(&d.path().join("m.json")).unwrap();
        let name = format!("TINKR_TEST_MANIFEST_{}", std::process::id());
        cs.set_env(&RegistryEnv, &name, &EnvValue::plain("1"))
            .unwrap();
        assert!(RegistryEnv.get(&name).unwrap().is_some());
        cs.undo(&RegistryEnv).unwrap();
        assert!(RegistryEnv.get(&name).unwrap().is_none());
    }
}
