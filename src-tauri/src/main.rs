// release 构建不弹控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Codex 的 hook 以转发模式启动本程序：转发完就退出，不启动桌宠
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = perch_lib::agents::hook::run_from_args(&args) {
        std::process::exit(code);
    }
    perch_lib::run()
}
