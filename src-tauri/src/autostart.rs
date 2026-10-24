//! 开机自启动：在 HKCU\Software\Microsoft\Windows\CurrentVersion\Run 写一项，只对当前用户生效，不需要管理员权限。
//! 开没开以注册表为准（不另存一份配置），默认不写。

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Perch";

/// 自启动时运行的命令：带引号的当前程序路径
fn command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("找不到程序路径：{e}"))?;
    Ok(format!("\"{}\"", exe.display()))
}

pub fn is_enabled() -> bool {
    read(RUN_KEY, VALUE_NAME).is_some()
}

pub fn set(on: bool) -> Result<(), String> {
    if on {
        write(RUN_KEY, VALUE_NAME, &command()?)
    } else {
        remove(RUN_KEY, VALUE_NAME)
    }
}

#[cfg(windows)]
fn read(key: &str, name: &str) -> Option<String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut buf = vec![0u16; 4096];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut len))
    };
    if r.is_err() {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..n.min(buf.len())]))
}

#[cfg(windows)]
fn write(key: &str, name: &str, value: &str) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let r = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name), REG_SZ.0, Some(data.as_ptr() as *const _), (data.len() * 2) as u32)
    };
    r.ok().map_err(|e| format!("写入开机启动项失败：{e}"))
}

#[cfg(windows)]
fn remove(key: &str, name: &str) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{RegDeleteKeyValueW, HKEY_CURRENT_USER};
    let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name)) };
    // 本来就没有也算成功
    if r.is_ok() || r == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("删除开机启动项失败：{}", r.to_hresult().message()))
    }
}

#[cfg(not(windows))]
fn read(_key: &str, _name: &str) -> Option<String> {
    None
}
#[cfg(not(windows))]
fn write(_key: &str, _name: &str, _value: &str) -> Result<(), String> {
    Err("开机自启动只支持 Windows".into())
}
#[cfg(not(windows))]
fn remove(_key: &str, _name: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在一个临时的注册表键下走一遍写、读、删（不碰真正的 Run 键），最后把临时键删掉
    #[test]
    fn write_read_remove_roundtrip() {
        let key = format!(r"Software\Perch-test-{}", uuid::Uuid::new_v4().simple());
        assert_eq!(read(&key, VALUE_NAME), None);
        let cmd = command().unwrap();
        assert!(cmd.starts_with('"') && cmd.ends_with('"'));
        write(&key, VALUE_NAME, &cmd).unwrap();
        assert_eq!(read(&key, VALUE_NAME).as_deref(), Some(cmd.as_str()));
        remove(&key, VALUE_NAME).unwrap();
        assert_eq!(read(&key, VALUE_NAME), None);
        // 删不存在的项也算成功
        remove(&key, VALUE_NAME).unwrap();
        #[cfg(windows)]
        unsafe {
            use windows::core::HSTRING;
            use windows::Win32::System::Registry::{RegDeleteKeyW, HKEY_CURRENT_USER};
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(key.as_str()));
        }
    }
}
