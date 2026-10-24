fn main() {
    // 语音识别的 sherpa-onnx DLL 延迟加载：程序启动时不需要它，开启语音输入后才按完整路径加载（见 src/voice/runtime.rs）
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:sherpa-onnx-c-api.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }
    tauri_build::build()
}
