//! 内置文件工具：read_file / write_file / edit_file（不提供 bash）。
//! 所有路径都必须落在白名单目录内；写入和修改先生成 Change（含 diff），经用户确认后再 apply。

use encoding_rs::{Encoding, GBK, UTF_8};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

const MAX_READ_BYTES: u64 = 5 * 1024 * 1024;
const DEFAULT_LINES: usize = 400;
const MAX_READ_CHARS: usize = 60_000;
const MAX_DIR_ENTRIES: usize = 500;
const MAX_DIFF_CHARS: usize = 20_000;

/// 提供给模型的工具定义（OpenAI tools 格式）
pub fn definitions(with_image: bool) -> Value {
    let mut tools = vec![
        json!({ "type": "function", "function": {
            "name": "read_file",
            "description": "读取文本文件内容（带行号）。如果 path 是目录，则返回目录下的文件列表。只能访问用户允许的目录。",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string", "description": "文件或目录的绝对路径" },
                "offset": { "type": "integer", "description": "从第几行开始读（1 起），默认 1" },
                "limit": { "type": "integer", "description": "最多读几行，默认 400" }
            }, "required": ["path"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "write_file",
            "description": "创建新文件或整体覆盖已有文件。会先请用户确认。修改已有文件的一部分时优先用 edit_file。",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string", "description": "文件的绝对路径" },
                "content": { "type": "string", "description": "完整的文件内容" }
            }, "required": ["path", "content"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "edit_file",
            "description": "把文件中的 old_string 精确替换为 new_string。old_string 必须和文件内容逐字一致（含缩进），且在文件中只出现一次，除非 replace_all 为 true。会先请用户确认。修改前先用 read_file 看清原文。",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string", "description": "文件的绝对路径" },
                "old_string": { "type": "string", "description": "要被替换的原文" },
                "new_string": { "type": "string", "description": "替换后的内容" },
                "replace_all": { "type": "boolean", "description": "是否替换所有出现的位置，默认 false" }
            }, "required": ["path", "old_string", "new_string"] }
        }}),
    ];
    if with_image {
        tools.push(json!({ "type": "function", "function": {
            "name": "generate_image",
            "description": "根据描述生成一张图片。只在用户明确要求画图、生成图片时使用。",
            "parameters": { "type": "object", "properties": {
                "prompt": { "type": "string", "description": "详细的画面描述（主体、风格、构图、色调），用用户的语言或英文" }
            }, "required": ["prompt"] }
        }}));
    }
    Value::Array(tools)
}

/// 去掉 Windows canonicalize 加上的 \\?\ 前缀，方便给人看
pub fn display(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 把配置里的白名单目录规范化（不存在的忽略）
pub fn canonical_allowed(dirs: &[String]) -> Vec<PathBuf> {
    dirs.iter().filter_map(|d| Path::new(d).canonicalize().ok()).collect()
}

/// 解析路径并检查白名单：处理相对路径、`..`、符号链接。
/// must_exist=false 时允许路径还不存在（新建文件），但已存在的最近祖先必须在白名单内。
pub fn resolve(path: &str, allowed: &[PathBuf], must_exist: bool) -> Result<PathBuf, String> {
    if allowed.is_empty() {
        return Err("用户还没有设置允许访问的目录（设置 → 工具）".into());
    }
    let raw = PathBuf::from(path.trim());
    let joined = if raw.is_absolute() { raw } else { allowed[0].join(raw) };
    let norm = lexical_normalize(&joined);

    let mut existing = norm.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or("路径无效")?.to_owned();
        rest.push(name);
        existing = existing.parent().ok_or("路径无效")?.to_path_buf();
    }
    if must_exist && !rest.is_empty() {
        return Err(format!("文件不存在：{}", display(&norm)));
    }
    let mut canon = existing.canonicalize().map_err(|e| format!("无法访问 {}：{e}", display(&existing)))?;
    for r in rest.iter().rev() {
        canon.push(r);
    }
    if !allowed.iter().any(|a| canon.starts_with(a)) {
        return Err(format!("{} 不在允许访问的目录里，拒绝访问", display(&canon)));
    }
    Ok(canon)
}

/// 识别文本编码：UTF-8（可带 BOM）优先，不是的话试 GBK（老版记事本、很多中文软件导出的文件）
pub fn decode_text(bytes: &[u8]) -> Result<(String, &'static Encoding), String> {
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err("这是二进制文件（或 UTF-16 编码），不能按文本读取".into());
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok((s.to_string(), UTF_8));
    }
    GBK.decode_without_bom_handling_and_without_replacement(bytes)
        .map(|s| (s.into_owned(), GBK))
        .ok_or_else(|| "文件既不是 UTF-8 也不是 GBK 编码，无法读取".to_string())
}

fn read_text(path: &Path) -> Result<(String, &'static Encoding), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_READ_BYTES {
        return Err(format!("文件太大（{} KB），超过 5MB 上限", meta.len() / 1024));
    }
    decode_text(&std::fs::read(path).map_err(|e| e.to_string())?)
}

/// 保持原编码写回时，新内容必须能用这种编码表示（GBK 存不了 emoji 等字符）
fn check_encodable(text: &str, enc: &'static Encoding) -> Result<(), String> {
    if enc == GBK && GBK.encode(text).2 {
        let bad: String = text.chars().filter(|c| GBK.encode(&c.to_string()).2).take(5).collect();
        return Err(format!("这个文件是 GBK 编码，新内容里有 GBK 无法保存的字符（{bad}），请去掉这些字符再试"));
    }
    Ok(())
}

pub fn read(path: &Path, offset: Option<usize>, limit: Option<usize>) -> Result<String, String> {
    if path.is_dir() {
        let mut entries: Vec<(bool, String, u64)> = std::fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|e| {
                let md = e.metadata().ok();
                (md.as_ref().is_some_and(|m| m.is_dir()), e.file_name().to_string_lossy().to_string(), md.map_or(0, |m| m.len()))
            })
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let total = entries.len();
        let mut out = format!("目录 {}，共 {total} 项：\n", display(path));
        for (is_dir, name, size) in entries.into_iter().take(MAX_DIR_ENTRIES) {
            if is_dir {
                out.push_str(&format!("{name}/\n"));
            } else {
                out.push_str(&format!("{name}  ({size} 字节)\n"));
            }
        }
        if total > MAX_DIR_ENTRIES {
            out.push_str(&format!("……还有 {} 项未列出\n", total - MAX_DIR_ENTRIES));
        }
        return Ok(out);
    }
    let (text, enc) = read_text(path)?;
    let lines: Vec<&str> = text.lines().collect();
    let start = offset.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(DEFAULT_LINES).clamp(1, 5000);
    let mut out = String::new();
    let mut shown_end = start.saturating_sub(1);
    for (i, line) in lines.iter().enumerate().skip(start - 1).take(limit) {
        if out.len() > MAX_READ_CHARS {
            break;
        }
        out.push_str(&format!("{:>6}\t{}\n", i + 1, line));
        shown_end = i + 1;
    }
    let enc_note = if enc == GBK { "（GBK 编码）" } else { "" };
    let header = format!("文件 {}{enc_note}，共 {} 行，显示第 {}-{} 行", display(path), lines.len(), start.min(lines.len().max(1)), shown_end);
    let more = if shown_end < lines.len() { format!("\n（还有 {} 行未显示，可用 offset 继续读）", lines.len() - shown_end) } else { String::new() };
    Ok(format!("{header}\n{out}{more}"))
}

/// 待确认的文件修改
#[derive(Debug)]
pub struct Change {
    pub path: PathBuf,
    /// None 表示新建文件
    pub old: Option<String>,
    pub new: String,
    /// 写回时用的编码：沿用原文件的（UTF-8 或 GBK），新文件用 UTF-8
    pub encoding: &'static Encoding,
}

impl Change {
    pub fn diff(&self) -> String {
        let old = self.old.as_deref().unwrap_or("");
        let name = display(&self.path);
        let d = similar::TextDiff::from_lines(old, &self.new)
            .unified_diff()
            .context_radius(3)
            .header(if self.old.is_some() { &name } else { "/dev/null" }, &name)
            .to_string();
        if d.chars().count() > MAX_DIFF_CHARS {
            let cut: String = d.chars().take(MAX_DIFF_CHARS).collect();
            format!("{cut}\n……（diff 太长，只显示前 {MAX_DIFF_CHARS} 字）")
        } else {
            d
        }
    }

    /// (新增行数, 删除行数)
    pub fn stats(&self) -> (usize, usize) {
        let d = similar::TextDiff::from_lines(self.old.as_deref().unwrap_or(""), &self.new);
        d.iter_all_changes().fold((0, 0), |(a, r), c| match c.tag() {
            similar::ChangeTag::Insert => (a + 1, r),
            similar::ChangeTag::Delete => (a, r + 1),
            similar::ChangeTag::Equal => (a, r),
        })
    }

    pub fn apply(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = if self.encoding == GBK {
            let (b, _, bad) = GBK.encode(&self.new);
            if bad {
                return Err("新内容里有 GBK 无法保存的字符".into());
            }
            b.into_owned()
        } else {
            self.new.clone().into_bytes()
        };
        std::fs::write(&self.path, bytes).map_err(|e| format!("写入失败：{e}"))
    }
}

pub fn plan_write(path: &Path, content: &str) -> Result<Change, String> {
    if path.is_dir() {
        return Err("这是一个目录，不能写入".into());
    }
    if !path.exists() {
        return Ok(Change { path: path.to_path_buf(), old: None, new: content.to_string(), encoding: UTF_8 });
    }
    let (old, enc) = read_text(path)?;
    let mut new = content.to_string();
    // 原文件带 BOM 的，整体覆盖时保留 BOM
    if old.starts_with('\u{feff}') && !new.starts_with('\u{feff}') {
        new.insert(0, '\u{feff}');
    }
    check_encodable(&new, enc)?;
    Ok(Change { path: path.to_path_buf(), old: Some(old), new, encoding: enc })
}

pub fn plan_edit(path: &Path, old_string: &str, new_string: &str, replace_all: bool) -> Result<Change, String> {
    if old_string.is_empty() {
        return Err("old_string 不能为空；新建文件请用 write_file".into());
    }
    let (text, enc) = read_text(path)?;
    // 模型给的通常是 \n，文件可能是 \r\n
    let (old_s, new_s) = if !text.contains(old_string) && text.contains("\r\n") && !old_string.contains("\r\n") {
        (old_string.replace('\n', "\r\n"), new_string.replace('\n', "\r\n"))
    } else {
        (old_string.to_string(), new_string.to_string())
    };
    let count = text.matches(old_s.as_str()).count();
    if count == 0 {
        return Err("在文件里没有找到 old_string，请先用 read_file 确认原文（包括缩进和空格）".into());
    }
    if count > 1 && !replace_all {
        return Err(format!("old_string 在文件里出现了 {count} 次，请提供更多上下文让它唯一，或者设置 replace_all=true"));
    }
    let new = if replace_all { text.replace(old_s.as_str(), &new_s) } else { text.replacen(old_s.as_str(), &new_s, 1) };
    check_encodable(&new, enc)?;
    Ok(Change { path: path.to_path_buf(), old: Some(text), new, encoding: enc })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (PathBuf, Vec<PathBuf>) {
        let root = std::env::temp_dir().join(format!("tp-tools-{}", uuid::Uuid::new_v4()));
        let allowed = root.join("allowed");
        std::fs::create_dir_all(allowed.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(allowed.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(root.join("secret").join("s.txt"), "secret").unwrap();
        let canon = canonical_allowed(&[allowed.to_string_lossy().to_string()]);
        (root, canon)
    }

    #[test]
    fn whitelist_blocks_escape() {
        let (root, allowed) = sandbox();
        let a = &allowed[0];
        assert!(resolve(&a.join("a.txt").to_string_lossy(), &allowed, true).is_ok());
        assert!(resolve("a.txt", &allowed, true).is_ok(), "相对路径按第一个白名单目录解析");
        assert!(resolve(&a.join("..").join("secret").join("s.txt").to_string_lossy(), &allowed, true).is_err());
        assert!(resolve(&root.join("secret").join("s.txt").to_string_lossy(), &allowed, true).is_err());
        assert!(resolve(&a.join("new").join("n.txt").to_string_lossy(), &allowed, false).is_ok(), "白名单内可以新建");
        assert!(resolve(&a.join("missing.txt").to_string_lossy(), &allowed, true).is_err());
        assert!(resolve("x", &[], true).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_lists_dirs_and_numbers_lines() {
        let (root, allowed) = sandbox();
        let listing = read(&allowed[0], None, None).unwrap();
        assert!(listing.contains("sub/") && listing.contains("a.txt"));
        let text = read(&allowed[0].join("a.txt"), Some(2), Some(1)).unwrap();
        assert!(text.contains("     2\ttwo") && !text.contains("three\n     ") && text.contains("还有 1 行"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn edit_requires_unique_match() {
        let (root, allowed) = sandbox();
        let p = allowed[0].join("b.txt");
        std::fs::write(&p, "x = 1\r\ny = 1\r\n").unwrap();
        assert!(plan_edit(&p, "= 1", "= 2", false).unwrap_err().contains("出现了 2 次"));
        assert_eq!(plan_edit(&p, "= 1", "= 2", true).unwrap().new, "x = 2\r\ny = 2\r\n");
        // 模型用 \n，文件是 \r\n
        let c = plan_edit(&p, "x = 1\ny", "x = 3\ny", false).unwrap();
        assert_eq!(c.new, "x = 3\r\ny = 1\r\n");
        assert!(plan_edit(&p, "nope", "", false).is_err());
        c.apply().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "x = 3\r\ny = 1\r\n");
        assert_eq!(c.stats(), (1, 1));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_rejects_binary_non_utf8_and_huge() {
        let (root, allowed) = sandbox();
        let a = &allowed[0];
        std::fs::write(a.join("bin.dat"), [0x50u8, 0x4B, 0x00, 0x01, 0x02]).unwrap();
        assert!(read(&a.join("bin.dat"), None, None).unwrap_err().contains("二进制"));
        // 既不是 UTF-8 也不是合法 GBK
        std::fs::write(a.join("junk.txt"), [0xFFu8, 0xFF, 0xFF]).unwrap();
        assert!(read(&a.join("junk.txt"), None, None).unwrap_err().contains("GBK"));
        let big = std::fs::File::create(a.join("big.txt")).unwrap();
        big.set_len(MAX_READ_BYTES + 1).unwrap();
        assert!(read(&a.join("big.txt"), None, None).unwrap_err().contains("5MB"));
        // 带 BOM 的 UTF-8 能读
        std::fs::write(a.join("bom.txt"), "\u{feff}第一行\n").unwrap();
        assert!(read(&a.join("bom.txt"), None, None).unwrap().contains("第一行"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn write_and_edit_respect_whitelist() {
        let (root, allowed) = sandbox();
        let outside = root.join("secret").join("new.txt");
        assert!(resolve(&outside.to_string_lossy(), &allowed, false).unwrap_err().contains("不在允许"));
        assert!(resolve(&root.join("secret").join("s.txt").to_string_lossy(), &allowed, true).is_err());
        // 新建多层目录里的文件：解析通过，apply 时自动建目录
        let deep = resolve(&allowed[0].join("x").join("y").join("z.txt").to_string_lossy(), &allowed, false).unwrap();
        plan_write(&deep, "深层\n").unwrap().apply().unwrap();
        assert_eq!(std::fs::read_to_string(&deep).unwrap(), "深层\n");
        assert!(plan_write(&allowed[0].join("sub"), "x").unwrap_err().contains("目录"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn overwrite_diff_shows_removed_and_added() {
        let (root, allowed) = sandbox();
        let p = allowed[0].join("a.txt");
        let c = plan_write(&p, "one\n2\nthree\nfour\n").unwrap();
        let d = c.diff();
        assert!(d.contains("-two") && d.contains("+2") && d.contains("+four") && d.contains(" one"));
        assert_eq!(c.stats(), (2, 1));
        // 内容不变：没有增删
        assert_eq!(plan_write(&p, "one\ntwo\nthree\n").unwrap().stats(), (0, 0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn edit_multiline_keeps_rest_of_file() {
        let (root, allowed) = sandbox();
        let p = allowed[0].join("m.txt");
        std::fs::write(&p, "头\nfn a() {\n    1\n}\n尾\n").unwrap();
        let c = plan_edit(&p, "fn a() {\n    1\n}", "fn a() {\n    2\n}", false).unwrap();
        assert_eq!(c.new, "头\nfn a() {\n    2\n}\n尾\n");
        assert!(plan_edit(&p, "", "x", false).unwrap_err().contains("不能为空"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn huge_diff_is_truncated() {
        let (root, allowed) = sandbox();
        let big: String = (0..5000).map(|i| format!("第 {i} 行内容\n")).collect();
        let c = plan_write(&allowed[0].join("big.txt"), &big).unwrap();
        let d = c.diff();
        assert!(d.contains("diff 太长"));
        assert!(d.chars().count() < MAX_DIFF_CHARS + 100);
        assert_eq!(c.stats(), (5000, 0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn crlf_diff_marks_only_changed_line() {
        let (root, allowed) = sandbox();
        let p = allowed[0].join("w.txt");
        std::fs::write(&p, "a\r\nb\r\nc\r\n").unwrap();
        let c = plan_edit(&p, "b", "B", false).unwrap();
        assert_eq!(c.stats(), (1, 1));
        // 按行检查（文件头里的临时路径带随机 uuid，不能直接查子串）
        let changed: Vec<String> = c
            .diff()
            .lines()
            .filter(|l| (l.starts_with('-') && !l.starts_with("---")) || (l.starts_with('+') && !l.starts_with("+++")))
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        assert_eq!(changed, vec!["-b", "+B"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn gbk_files_read_and_written_back_as_gbk() {
        let (root, allowed) = sandbox();
        let p = allowed[0].join("gbk.txt");
        // "你好\r\n世界\r\n" 的 GBK 编码
        let (orig, _, _) = GBK.encode("你好\r\n世界\r\n");
        std::fs::write(&p, &orig).unwrap();
        let shown = read(&p, None, None).unwrap();
        assert!(shown.contains("GBK") && shown.contains("你好") && shown.contains("世界"));
        let c = plan_edit(&p, "世界", "桌宠", false).unwrap();
        assert_eq!(c.stats(), (1, 1));
        c.apply().unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert!(std::str::from_utf8(&bytes).is_err(), "写回后仍是 GBK，不是 UTF-8");
        assert_eq!(GBK.decode(&bytes).0, "你好\r\n桌宠\r\n");
        // GBK 存不了 emoji：在确认前就报错，文件不动
        assert!(plan_edit(&p, "桌宠", "桌宠😀", false).unwrap_err().contains("😀"));
        assert!(plan_write(&p, "😀").unwrap_err().contains("GBK"));
        // 新建文件用 UTF-8
        let n = plan_write(&allowed[0].join("new.txt"), "😀\n").unwrap();
        assert_eq!(n.encoding, UTF_8);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn utf8_bom_kept_on_overwrite() {
        let (root, allowed) = sandbox();
        let p = allowed[0].join("bom.txt");
        std::fs::write(&p, "\u{feff}旧\n").unwrap();
        plan_write(&p, "新\n").unwrap().apply().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "\u{feff}新\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn write_new_file_diff() {
        let (root, allowed) = sandbox();
        let c = plan_write(&allowed[0].join("new.md"), "hello\n").unwrap();
        assert!(c.old.is_none());
        assert!(c.diff().contains("+hello"));
        let _ = std::fs::remove_dir_all(root);
    }
}
