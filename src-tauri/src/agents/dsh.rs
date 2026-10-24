//! DeepSeek Harness 接入：生成桌宠插件（一个 DSH 组合包），用 DSH 官方的 `dsh plugin` 命令装进 desktop profile，
//! 再把它加进 profile 的 `dsh.profile.bundles`。
//! DSH 只加载 `dsh.profile.bundles` 里列出、并在 package.json 里声明了 `dsh.bundle` 的包；
//! `dsh plugin add` 只是转给 pnpm 安装依赖，不会改这个列表。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const PLUGIN_NAME: &str = "perch-dsh-bridge";
/// 改名前的插件名；接入时把它从 profile 里清掉
const LEGACY_PLUGIN_NAME: &str = "table-pet-dsh-bridge";
const INDEX_JS: &str = include_str!("../../resources/dsh-bridge/index.js");
const PACKAGE_JSON: &str = include_str!("../../resources/dsh-bridge/package.json");
const PATCH_YML: &str = include_str!("../../resources/dsh-bridge/cordis.patch.yml");
const LOCALE_ZH: &str = include_str!("../../resources/dsh-bridge/locale/zh.json");
const LOCALE_EN: &str = include_str!("../../resources/dsh-bridge/locale/en.json");

/// 插件目录里由桌宠生成的固定文件（config.json 另外写）
const FILES: &[(&str, &str)] = &[
    ("index.js", INDEX_JS),
    ("package.json", PACKAGE_JSON),
    ("cordis.patch.yml", PATCH_YML),
    ("locale/zh.json", LOCALE_ZH),
    ("locale/en.json", LOCALE_EN),
];

pub fn plugin_dir() -> PathBuf {
    crate::config::data_dir().join("integrations").join("dsh-bridge")
}

pub fn dsh_home() -> PathBuf {
    std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".dsh"))
}

/// DSH 桌面版至少打开过一次（profile 存在）
pub fn desktop_profile_exists() -> bool {
    dsh_home().join("profiles").join("desktop").join("package.json").is_file()
}

/// 写出插件文件；config.json 带端口、令牌、是否附带最后回复
pub fn write_plugin(port: u16, token: &str, summarize: bool) -> Result<PathBuf, String> {
    let dir = plugin_dir();
    std::fs::create_dir_all(dir.join("locale")).map_err(|e| e.to_string())?;
    for (name, text) in FILES {
        std::fs::write(dir.join(name), text).map_err(|e| e.to_string())?;
    }
    let cfg = json!({ "port": port, "token": token, "summarize": summarize });
    std::fs::write(dir.join("config.json"), serde_json::to_string_pretty(&cfg).unwrap()).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn profile_manifest_path() -> PathBuf {
    dsh_home().join("profiles").join("desktop").join("package.json")
}

fn read_profile_manifest() -> Option<Value> {
    std::fs::read_to_string(profile_manifest_path()).ok().and_then(|t| serde_json::from_str(&t).ok())
}

fn bundles_of(v: &Value) -> Vec<String> {
    v.pointer("/dsh/profile/bundles")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default()
}

fn has_dependency(name: &str) -> bool {
    read_profile_manifest().is_some_and(|v| v.get("dependencies").and_then(|d| d.get(name)).is_some())
}

fn in_bundles(name: &str) -> bool {
    read_profile_manifest().is_some_and(|v| bundles_of(&v).iter().any(|b| b == name))
}

/// 已作为依赖装进 desktop profile（看 profile 的 package.json 依赖）
pub fn installed_in_profile() -> bool {
    has_dependency(PLUGIN_NAME)
}

/// 已在 profile 的组合包列表里（DSH 只加载列表里的组合包）
pub fn enabled_in_profile() -> bool {
    in_bundles(PLUGIN_NAME)
}

/// 清掉改名前装的插件（它链接的目录已经随数据目录搬走了）
fn remove_legacy(dsh: &Path) -> Result<(), String> {
    if in_bundles(LEGACY_PLUGIN_NAME) {
        set_bundle_enabled(LEGACY_PLUGIN_NAME, false)?;
    }
    if has_dependency(LEGACY_PLUGIN_NAME) {
        run_dsh(dsh, &["plugin", "--profile", "desktop", "remove", LEGACY_PLUGIN_NAME])?;
    }
    Ok(())
}

/// 把插件加进 / 移出 profile 的 `dsh.profile.bundles`（保留其余内容和键顺序；第一次改之前备份）
fn set_bundle_enabled(name: &str, on: bool) -> Result<(), String> {
    let path = profile_manifest_path();
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    let mut v: Value = serde_json::from_str(&text).map_err(|e| format!("{} 不是有效的 JSON：{e}", path.display()))?;
    let mut list = bundles_of(&v);
    let has = list.iter().any(|b| b == name);
    if has == on {
        return Ok(());
    }
    if on {
        list.push(name.to_string());
    } else {
        list.retain(|b| b != name);
    }
    let root = v.as_object_mut().ok_or("profile 的 package.json 不是对象")?;
    let dsh = root.entry("dsh").or_insert_with(|| json!({}));
    let profile = dsh.as_object_mut().ok_or("dsh 字段不是对象")?.entry("profile").or_insert_with(|| json!({}));
    profile.as_object_mut().ok_or("dsh.profile 字段不是对象")?.insert("bundles".into(), json!(list));
    let bak = path.with_extension("json.perch-backup");
    if !bak.exists() && !path.with_extension("json.table-pet-backup").exists() {
        std::fs::write(&bak, &text).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.perch-tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap() + "\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// 插件里的配置和当前一致
pub fn plugin_config_current(port: u16, token: &str, summarize: bool) -> bool {
    std::fs::read_to_string(plugin_dir().join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| v == json!({ "port": port, "token": token, "summarize": summarize }))
        && FILES.iter().all(|(name, text)| std::fs::read_to_string(plugin_dir().join(name)).is_ok_and(|t| t == *text))
}

#[cfg(windows)]
fn registry_path(root: windows::Win32::System::Registry::HKEY, key: &str) -> String {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ};
    let mut buf = vec![0u16; 32768];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from("Path"),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    };
    if r.is_err() {
        return String::new();
    }
    let n = (len as usize / 2).saturating_sub(1);
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

/// 找 dsh 命令。用注册表里的 PATH（桌宠启动后才装的命令，进程自己的 PATH 里还没有）
pub fn find_dsh() -> Option<PathBuf> {
    let mut paths = std::env::var("PATH").unwrap_or_default();
    #[cfg(windows)]
    {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        paths = format!(
            "{};{};{}",
            registry_path(HKEY_CURRENT_USER, "Environment"),
            registry_path(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
            paths
        );
    }
    paths
        .split(';')
        .map(|p| PathBuf::from(p.trim().trim_matches('"')))
        .filter(|p| !p.as_os_str().is_empty())
        .flat_map(|d| ["dsh.cmd", "dsh.exe", "dsh.bat"].map(|f| d.join(f)))
        .find(|f| f.is_file())
}

/// 运行 `dsh <args>`；.cmd 要经过 cmd.exe
fn run_dsh(dsh: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("cmd");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let quoted: Vec<String> = std::iter::once(dsh.display().to_string())
            .chain(args.iter().map(|a| a.to_string()))
            .map(|a| format!("\"{a}\""))
            .collect();
        // /S + 外层引号：cmd 只去掉最外面一对，里面每个参数的引号保留
        cmd.raw_arg(format!("/D /S /C \"{}\"", quoted.join(" ")));
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| format!("运行 dsh 失败：{e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!("dsh {} 失败：{}", args.join(" "), text.trim()))
    }
}

pub const NO_DSH_HINT: &str = "没有找到 dsh 命令。请在 DeepSeek Harness 菜单里点「管理 dsh 命令…」→ 安装，然后回来再试";

pub fn install(port: u16, token: &str, summarize: bool) -> Result<String, String> {
    if !desktop_profile_exists() {
        return Err("没有找到 DeepSeek Harness 桌面版的配置，请先打开一次 DeepSeek Harness".into());
    }
    let dsh = find_dsh().ok_or(NO_DSH_HINT)?;
    remove_legacy(&dsh)?;
    let dir = write_plugin(port, token, summarize)?;
    // 本地目录装成 link: 依赖，以后只改目录里的文件就行；已经装过就不再跑 pnpm
    let out = if installed_in_profile() {
        String::new()
    } else {
        run_dsh(&dsh, &["plugin", "--profile", "desktop", "add", &dir.display().to_string()])?
    };
    if !installed_in_profile() {
        return Err(format!("dsh 命令执行完了，但 profile 里没有看到插件：{}", out.trim()));
    }
    set_bundle_enabled(PLUGIN_NAME, true)?;
    Ok(out)
}

pub fn uninstall() -> Result<(), String> {
    for name in [PLUGIN_NAME, LEGACY_PLUGIN_NAME] {
        if in_bundles(name) {
            set_bundle_enabled(name, false)?;
        }
    }
    if !installed_in_profile() && !has_dependency(LEGACY_PLUGIN_NAME) {
        return Ok(());
    }
    let dsh = find_dsh().ok_or(NO_DSH_HINT)?;
    remove_legacy(&dsh)?;
    if !installed_in_profile() {
        return Ok(());
    }
    run_dsh(&dsh, &["plugin", "--profile", "desktop", "remove", PLUGIN_NAME]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实调用 dsh：装进临时 DSH_HOME 的 desktop profile，确认 DSH 组合里出现了插件条目，再卸载。
    /// 要本机装了 dsh 命令；不碰真实的 ~/.dsh。默认跳过。
    /// 运行：cargo test dsh_install_roundtrip -- --ignored --nocapture
    #[test]
    #[ignore]
    fn dsh_install_roundtrip() {
        // 临时目录用长路径：TEMP 常是 8.3 短名（如 C:\Users\ABCDEF~1），pnpm 前后看到两种写法会认为虚拟仓库位置变了
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let tmp = PathBuf::from(tmp.display().to_string().trim_start_matches(r"\\?\"));
        let root = tmp.join(format!("tp-dsh-{}", uuid::Uuid::new_v4()));
        let profile = root.join("home").join("profiles").join("desktop");
        std::fs::create_dir_all(&profile).unwrap();
        // 模拟改名前装过的旧插件：还在依赖和组合包列表里，但它链接的目录已经随数据目录搬走了
        let manifest = json!({
            "name": "dsh-profile-desktop",
            "private": true,
            "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app", LEGACY_PLUGIN_NAME] } },
            "dependencies": { LEGACY_PLUGIN_NAME: format!("link:{}", root.join("moved-away").display().to_string().replace('\\', "/")) }
        });
        std::fs::write(profile.join("package.json"), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        std::fs::write(profile.join("cordis.patch.yml"), "[]\n").unwrap();
        std::env::set_var("DSH_HOME", root.join("home"));
        std::env::set_var("PERCH_DATA_DIR", root.join("data"));

        // dsh 不让命令行直接读 desktop profile（归桌面版管），用一个指向它的目录联接换个名字读
        let alias = root.join("home").join("profiles").join("tpcheck");
        let ok = Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(&alias).arg(&profile).output().unwrap();
        assert!(ok.status.success());
        let dump = || {
            let out = run_dsh(&find_dsh().expect("没有 dsh 命令"), &["--profile", "tpcheck", "--dump-config"]).unwrap();
            out.lines().filter(|l| l.contains(PLUGIN_NAME)).map(str::trim).collect::<Vec<_>>().join(" | ")
        };
        assert!(dump().is_empty());
        install(47821, "tok", true).unwrap();
        assert!(installed_in_profile() && enabled_in_profile());
        assert!(plugin_config_current(47821, "tok", true));
        // 旧插件从依赖和组合包列表里都清掉了
        assert!(!has_dependency(LEGACY_PLUGIN_NAME) && !in_bundles(LEGACY_PLUGIN_NAME));
        let v = read_profile_manifest().unwrap();
        assert_eq!(bundles_of(&v), vec!["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app", PLUGIN_NAME]);
        let after = dump();
        eprintln!("组合里的插件条目：{after}");
        assert!(after.contains(PLUGIN_NAME));
        // 再装一次不重复
        install(47821, "tok", true).unwrap();
        assert_eq!(bundles_of(&read_profile_manifest().unwrap()).iter().filter(|b| *b == PLUGIN_NAME).count(), 1);

        uninstall().unwrap();
        assert!(!installed_in_profile() && !enabled_in_profile());
        assert!(dump().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
