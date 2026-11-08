//! 运行日志：正式版没有控制台，eprintln! 的输出原本哪儿也去不了，出了偶发问题无从查起。
//! 启动时把标准错误接到数据目录下的 logs/perch.log（超过 2 MB 就把旧的改名成 perch.old.log 再重新写）。
//! 从终端启动、标准错误本来就有去处时不接，照旧打印到终端。

const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub fn init() {
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE};

        let has_stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
        if has_stderr {
            return;
        }
        let dir = crate::config::data_dir().join("logs");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join("perch.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
            let _ = std::fs::rename(&path, dir.join("perch.old.log"));
        }
        let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
        let handle = HANDLE(file.into_raw_handle());
        if unsafe { SetStdHandle(STD_ERROR_HANDLE, handle) }.is_ok() {
            eprintln!("==== {} Perch {} 启动", stamp(), env!("CARGO_PKG_VERSION"));
        }
    }
}

/// 日志里的时间（按「常规」里的时区）
pub fn stamp() -> String {
    crate::clock::now().format("%m-%d %H:%M:%S").to_string()
}
