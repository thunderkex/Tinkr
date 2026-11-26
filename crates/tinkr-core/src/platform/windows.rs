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

fn apply_user_only_dacl(path: &Path) -> std::io::Result<()> {
    use core::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct SidAndAttributes {
        sid: *mut c_void,
        attributes: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(h: *mut c_void) -> i32;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(
            process_handle: *mut c_void,
            desired_access: u32,
            token_handle: *mut *mut c_void,
        ) -> i32;
        fn GetTokenInformation(
            token_handle: *mut c_void,
            token_information_class: u32,
            token_information: *mut u8,
            token_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
        fn GetLengthSid(sid: *mut c_void) -> u32;
        fn InitializeAcl(acl: *mut u8, acl_length: u32, acl_revision: u32) -> i32;
        fn AddAccessAllowedAceEx(
            acl: *mut u8,
            ace_revision: u32,
            ace_flags: u32,
            access_mask: u32,
            sid: *mut c_void,
        ) -> i32;
        fn SetNamedSecurityInfoW(
            object_name: *mut u16,
            object_type: u32,
            security_info: u32,
            psid_owner: *mut c_void,
            psid_group: *mut c_void,
            dacl: *mut u8,
            sacl: *mut u8,
        ) -> u32;
    }

    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER_CLASS: u32 = 1;
    const ACL_REVISION: u32 = 2;
    const OBJECT_INHERIT_ACE: u32 = 0x1;
    const CONTAINER_INHERIT_ACE: u32 = 0x2;
    const GENERIC_ALL: u32 = 0x1000_0000;
    const SE_FILE_OBJECT: u32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;

    let is_dir = path.is_dir();
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    unsafe {
        let mut token: *mut c_void = core::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TOKEN_USER_CLASS, core::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            CloseHandle(token);
            return Err(std::io::Error::last_os_error());
        }
        let mut token_buf = vec![0u8; needed as usize];
        if GetTokenInformation(
            token,
            TOKEN_USER_CLASS,
            token_buf.as_mut_ptr(),
            needed,
            &mut needed,
        ) == 0
        {
            CloseHandle(token);
            return Err(std::io::Error::last_os_error());
        }
        let user_sid = (*(token_buf.as_ptr() as *const SidAndAttributes)).sid;
        let sid_len = GetLengthSid(user_sid);
        let acl_len = ((16 + sid_len + 3) & !3).max(32);
        let mut acl_buf = vec![0u8; acl_len as usize];
        if InitializeAcl(acl_buf.as_mut_ptr(), acl_len, ACL_REVISION) == 0 {
            CloseHandle(token);
            return Err(std::io::Error::last_os_error());
        }
        let ace_flags = if is_dir {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            0
        };
        if AddAccessAllowedAceEx(
            acl_buf.as_mut_ptr(),
            ACL_REVISION,
            ace_flags,
            GENERIC_ALL,
            user_sid,
        ) == 0
        {
            CloseHandle(token);
            return Err(std::io::Error::last_os_error());
        }
        let status = SetNamedSecurityInfoW(
            wide.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            acl_buf.as_mut_ptr(),
            core::ptr::null_mut(),
        );
        CloseHandle(token);
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
    }
    Ok(())
}

impl Platform for Native {
    fn name(&self) -> &'static str {
        "windows"
    }
    fn set_private(&self, path: &Path) -> std::io::Result<()> {
        apply_user_only_dacl(path)
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
