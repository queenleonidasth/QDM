use std::path::{Path, PathBuf};

#[cfg(windows)]
pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
#[cfg(windows)]
fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    unsafe {
        let mut ptr = std::ptr::null_mut();
        if windows_sys::Win32::UI::Shell::SHGetKnownFolderPath(
            id,
            0,
            std::ptr::null_mut(),
            &mut ptr,
        ) < 0
        {
            return None;
        }
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1
        }
        let result = PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(
            ptr, len,
        )));
        windows_sys::Win32::System::Com::CoTaskMemFree(ptr.cast());
        Some(result)
    }
}
pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(p) = known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_RoamingAppData) {
            return p.join("QDM");
        }
    }
    std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir())
        .join("QDM")
}
/// Resolve an existing completed output without trusting a stale UI existence cache.
/// The fallback is limited to the recorded directory and a single filename.
pub fn existing_download_path(file_path: Option<&str>, directory: &str, filename: &str) -> Option<PathBuf> {
    if let Some(path) = file_path.filter(|path| !path.trim().is_empty()) {
        let candidate = Path::new(path);
        if candidate.is_file() { return Some(candidate.to_path_buf()); }
    }
    if filename.is_empty() || Path::new(filename).components().count() != 1 { return None; }
    let directory = Path::new(directory);
    if !directory.is_absolute() { return None; }
    let candidate = directory.join(filename);
    candidate.is_file().then_some(candidate)
}

pub fn download_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(p) = known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Downloads) {
            return p.join("QDM");
        }
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Downloads")
        .join("QDM")
}
pub fn extension_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default()
        .join("extensions")
}
pub fn error(message: &str) {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            wide(message).as_ptr(),
            wide("QDM").as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
        );
    }
    #[cfg(not(windows))]
    {
        eprintln!("{message}");
    }
}
#[cfg(windows)]
pub fn open(path: &str) -> Result<(), String> {
    if !Path::new(path).exists() {
        return Err("This file or folder is no longer available.".into());
    }
    unsafe {
        let result = windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            wide(path).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        ) as isize;
        if result <= 32 {
            Err(format!("Windows could not open this file ({result})."))
        } else {
            Ok(())
        }
    }
}
#[cfg(windows)]
pub fn reveal(path: &str) -> Result<(), String> {
    if Path::new(path).is_dir() {
        return open(path);
    }
    if !Path::new(path).exists() {
        return Path::new(path)
            .parent()
            .map(|p| open(&p.to_string_lossy()))
            .unwrap_or_else(|| Err("Folder is unavailable".into()));
    }
    let args = format!("/select,\"{}\"", path);
    unsafe {
        let result = windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            wide("explorer.exe").as_ptr(),
            wide(&args).as_ptr(),
            std::ptr::null(),
            1,
        ) as isize;
        if result <= 32 {
            Err(format!("Windows could not reveal the file ({result})."))
        } else {
            Ok(())
        }
    }
}
#[cfg(windows)]
pub struct SingleInstance(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
static INSTANCE_PROPERTY: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
#[cfg(windows)]
pub fn tag_window(hwnd: windows_sys::Win32::Foundation::HWND) {
    if let Some(name) = INSTANCE_PROPERTY.get() {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetPropW(hwnd, name.as_ptr(), 1usize as _);
        }
    }
}
#[cfg(windows)]
unsafe extern "system" fn restore_instance(
    hwnd: windows_sys::Win32::Foundation::HWND,
    param: isize,
) -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    if !GetPropW(hwnd, param as *const u16).is_null() {
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
        return 0;
    }
    1
}
#[cfg(windows)]
impl SingleInstance {
    pub fn acquire(data: &Path, restore: bool) -> Result<Option<Self>, String> {
        use sha2::{Digest, Sha256};
        use windows_sys::Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*};
        std::fs::create_dir_all(data).map_err(|e| e.to_string())?;
        let canonical = std::fs::canonicalize(data).map_err(|e| e.to_string())?;
        let digest = Sha256::digest(canonical.to_string_lossy().to_lowercase().as_bytes());
        let suffix = digest[..8]
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect::<String>();
        let property = wide(&format!("QDM.Native.Profile.{suffix}"));
        let _ = INSTANCE_PROPERTY.set(property.clone());
        unsafe {
            let handle = CreateMutexW(
                std::ptr::null(),
                0,
                wide(&format!("Local\\QDM.Native.{suffix}")).as_ptr(),
            );
            if handle.is_null() {
                return Err("Unable to create the QDM instance lock".into());
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                if restore {
                    EnumWindows(Some(restore_instance), property.as_ptr() as isize);
                }
                CloseHandle(handle);
                return if restore {
                    Ok(None)
                } else {
                    Err("This QDM data directory is already open in another process.".into())
                };
            }
            Ok(Some(Self(handle)))
        }
    }
}
#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(not(windows))]
pub fn protect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    Ok(bytes.to_vec())
}
#[cfg(not(windows))]
pub fn unprotect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    Ok(bytes.to_vec())
}
#[cfg(windows)]
fn crypt(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::*};
    if bytes.len() > u32::MAX as usize {
        return Err("Pairing token is too large".into());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        let ok = if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err("Windows could not protect the browser pairing token".into());
        }
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        Ok(result)
    }
}
#[cfg(windows)]
pub fn protect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    crypt(bytes, true)
}
#[cfg(windows)]
pub fn unprotect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    crypt(bytes, false)
}

#[cfg(test)]
mod output_resolution_tests {
    use super::*;

    #[test]
    fn resolves_recorded_file_and_directory_fallback_without_guessing_other_folders() {
        let directory = std::env::temp_dir().join(format!("qdm-output-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let filename = "present.bin";
        let actual = directory.join(filename);
        std::fs::write(&actual, b"test bytes").unwrap();
        let dir = directory.to_string_lossy();
        let stale = directory.join("missing.bin");
        assert_eq!(existing_download_path(Some(&actual.to_string_lossy()), &dir, filename), Some(actual.clone()));
        assert_eq!(existing_download_path(Some(&stale.to_string_lossy()), &dir, filename), Some(actual.clone()));
        assert_eq!(existing_download_path(None, &dir, filename), Some(actual.clone()));
        assert_eq!(existing_download_path(None, &dir, "absent.bin"), None);
        assert_eq!(existing_download_path(None, &dir, "../outside.bin"), None);
        std::fs::remove_file(&actual).unwrap();
        assert_eq!(existing_download_path(Some(&actual.to_string_lossy()), &dir, filename), None);
        std::fs::remove_dir(&directory).unwrap();
    }
}