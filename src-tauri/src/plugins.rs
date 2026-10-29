//! 渲染器插件：放在数据目录的 plugins/<文件夹>/ 下，plugin.json 声明它提供哪些渲染器。
//! 皮肤用到某个渲染器时，前端才来要插件代码（单个 ES 模块文件）并执行。
//! 插件格式见 docs/renderer-plugin.md。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    api_version: u32,
    renderers: Vec<String>,
    #[serde(default)]
    main: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCode {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub code: String,
}

pub fn plugins_dir() -> PathBuf {
    crate::config::data_dir().join("plugins")
}

fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(dir.join("plugin.json")).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// 找提供 kind 渲染器的插件，读出它的代码；没有就返回 None
pub fn renderer_plugin(root: &Path, kind: &str) -> Result<Option<PluginCode>, String> {
    let Ok(rd) = std::fs::read_dir(root) else { return Ok(None) };
    let mut dirs: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        let Some(m) = read_manifest(&dir) else { continue };
        if !m.renderers.iter().any(|r| r == kind) {
            continue;
        }
        let main = m.main.as_deref().unwrap_or("index.js");
        if Path::new(main).is_absolute() {
            return Err(format!("插件 {} 的 main 必须是相对路径", m.id));
        }
        // 入口文件不能跳出插件文件夹
        let canon_dir = dir.canonicalize().map_err(|e| e.to_string())?;
        let canon = dir
            .join(main)
            .canonicalize()
            .map_err(|e| format!("插件 {} 的入口 {main} 不存在: {e}", m.id))?;
        if !canon.starts_with(&canon_dir) {
            return Err(format!("插件 {} 的入口指向了插件文件夹之外", m.id));
        }
        let code = std::fs::read_to_string(&canon).map_err(|e| e.to_string())?;
        return Ok(Some(PluginCode {
            name: m.name.unwrap_or_else(|| m.id.clone()),
            version: m.version.unwrap_or_default(),
            id: m.id,
            api_version: m.api_version,
            code,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_plugin_by_renderer() {
        let root = std::env::temp_dir().join(format!("perch-plugin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a = root.join("a");
        let b = root.join("b");
        let bad = root.join("c-bad");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(b.join("dist")).unwrap();
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(a.join("plugin.json"), r#"{"id":"a","apiVersion":1,"renderers":["live2d"]}"#).unwrap();
        std::fs::write(a.join("index.js"), "export default () => {}").unwrap();
        std::fs::write(
            b.join("plugin.json"),
            r#"{"id":"perch-spine","name":"Spine","version":"0.1.0","apiVersion":1,"renderers":["spine"],"main":"dist/spine.js"}"#,
        )
        .unwrap();
        std::fs::write(b.join("dist").join("spine.js"), "// spine").unwrap();
        std::fs::write(bad.join("plugin.json"), r#"{"id":"bad","apiVersion":1,"renderers":["evil"],"main":"../a/index.js"}"#).unwrap();

        let p = renderer_plugin(&root, "spine").unwrap().unwrap();
        assert_eq!((p.id.as_str(), p.name.as_str(), p.version.as_str(), p.api_version), ("perch-spine", "Spine", "0.1.0", 1));
        assert_eq!(p.code, "// spine");
        assert_eq!(renderer_plugin(&root, "live2d").unwrap().unwrap().name, "a");
        assert!(renderer_plugin(&root, "none").unwrap().is_none());
        assert!(renderer_plugin(&root, "evil").is_err());
        assert!(renderer_plugin(&root.join("missing"), "spine").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
