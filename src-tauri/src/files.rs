//! 附件处理：图片存一份副本；文本、PDF、Word 提取文字并按上限截断。

use crate::store::AttachmentMeta;
use base64::Engine;
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "markdown", "json", "jsonl", "csv", "tsv", "log", "xml", "yaml", "yml", "toml", "ini", "cfg",
    "conf", "html", "htm", "css", "js", "jsx", "ts", "tsx", "py", "rs", "go", "java", "kt", "c", "h", "cpp",
    "hpp", "cs", "rb", "php", "sh", "ps1", "bat", "sql", "lua", "swift", "vue", "svelte", "srt", "env",
];

#[derive(Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AttachmentIn {
    /// 拖入或选择的本地文件
    Path { path: String },
    /// 粘贴的图片
    #[serde(rename_all = "camelCase")]
    ImageData { name: String, mime: String, data: String },
}

pub struct Processed {
    pub meta: AttachmentMeta,
    /// 文件内容，拼进发给模型的文字
    pub text_block: Option<String>,
}

pub fn attachments_dir() -> PathBuf {
    crate::config::data_dir().join("attachments")
}

fn ext_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

pub fn save_image(bytes: &[u8], ext: &str) -> Result<String, String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("图片超过 20MB".into());
    }
    let dir = attachments_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = format!("{}.{}", uuid::Uuid::new_v4(), ext);
    std::fs::write(dir.join(&file), bytes).map_err(|e| e.to_string())?;
    Ok(file)
}

/// 读取 attachments 目录里的图片；只接受纯文件名，防止路径穿越
pub fn read_saved(file: &str) -> Result<Vec<u8>, String> {
    if file.contains(['/', '\\']) || file.contains("..") {
        return Err("非法的附件名".into());
    }
    std::fs::read(attachments_dir().join(file)).map_err(|e| e.to_string())
}

pub fn image_data_url(file: &str) -> Result<String, String> {
    let bytes = read_saved(file)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{};base64,{}", mime_for(&ext_of(file)), b64))
}

fn truncate(text: String, limit: usize) -> (String, Option<String>) {
    let total = text.chars().count();
    if total <= limit {
        return (text, None);
    }
    let kept: String = text.chars().take(limit).collect();
    let note = format!("内容过长，已截断：共 {total} 字，只保留前 {limit} 字");
    (kept, Some(note))
}

fn extract_pdf(path: &Path) -> Result<String, String> {
    let path = path.to_path_buf();
    // pdf-extract 遇到少见的 PDF 结构可能 panic，兜住它
    std::panic::catch_unwind(move || pdf_extract::extract_text(&path))
        .map_err(|_| "PDF 解析失败".to_string())?
        .map_err(|e| format!("PDF 解析失败: {e}"))
}

fn extract_docx(path: &Path) -> Result<String, String> {
    use quick_xml::events::Event;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("不是有效的 docx: {e}"))?;
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .map_err(|_| "docx 里没有 word/document.xml".to_string())?
        .read_to_string(&mut xml)
        .map_err(|e| e.to_string())?;

    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == "w:t" => in_text = true,
            Ok(Event::End(e)) => match e.name().as_ref() {
                "w:t" => in_text = false,
                "w:p" => out.push('\n'),
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                "w:tab" => out.push('\t'),
                "w:br" => out.push('\n'),
                _ => {}
            },
            Ok(Event::Text(t)) if in_text => {
                let raw: String = AsRef::<str>::as_ref(&t).to_string();
                out.push_str(&quick_xml::escape::unescape(&raw).map(|c| c.into_owned()).unwrap_or(raw));
            }
            Ok(Event::GeneralRef(r)) if in_text => {
                // 实体引用（&amp; 等）在新版 quick-xml 里单独给出
                let name: String = AsRef::<str>::as_ref(&r).to_string();
                let entity = format!("&{name};");
                out.push_str(&quick_xml::escape::unescape(&entity).map(|c| c.into_owned()).unwrap_or(entity));
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("docx 解析失败: {e}")),
            _ => {}
        }
    }
    Ok(out)
}

fn file_block(name: &str, text: &str, note: Option<&str>) -> String {
    let mut block = format!("【附件：{name}】\n{text}");
    if let Some(n) = note {
        block.push_str(&format!("\n【{n}】"));
    }
    block
}

pub fn process(att: AttachmentIn, file_chars: usize) -> Result<Processed, String> {
    match att {
        AttachmentIn::ImageData { name, mime, data } => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.split(',').next_back().unwrap_or(""))
                .map_err(|e| format!("图片数据无效: {e}"))?;
            let ext = match mime.as_str() {
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                "image/gif" => "gif",
                _ => "png",
            };
            let file = save_image(&bytes, ext)?;
            Ok(Processed {
                meta: AttachmentMeta { kind: "image".into(), name, file: Some(file), desc: None, note: None, call: None },
                text_block: None,
            })
        }
        AttachmentIn::Path { path } => {
            let p = PathBuf::from(&path);
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(path.clone());
            let meta_len = std::fs::metadata(&p).map_err(|e| format!("{name}: {e}"))?.len();
            if meta_len > MAX_FILE_BYTES {
                return Err(format!("{name}: 文件超过 50MB"));
            }
            let ext = ext_of(&name);
            if IMAGE_EXTS.contains(&ext.as_str()) {
                let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
                let file = save_image(&bytes, &ext)?;
                return Ok(Processed {
                    meta: AttachmentMeta { kind: "image".into(), name, file: Some(file), desc: None, note: None, call: None },
                    text_block: None,
                });
            }
            let text = match ext.as_str() {
                "pdf" => {
                    let t = extract_pdf(&p)?;
                    if t.trim().is_empty() {
                        return Err(format!("{name}: 这个 PDF 没有文字层（可能是扫描件），请截图后发送"));
                    }
                    t
                }
                "docx" => extract_docx(&p)?,
                "doc" | "xls" | "xlsx" | "ppt" | "pptx" => {
                    return Err(format!("{name}: 暂不支持这种格式，Word 请另存为 .docx"));
                }
                _ => {
                    let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
                    match crate::tools::decode_text(&bytes) {
                        Ok((text, _)) => text,
                        // 扩展名明确是文本的，尽量按 UTF-8 读出来；否则当作不支持的格式
                        Err(_) if TEXT_EXTS.contains(&ext.as_str()) => String::from_utf8_lossy(&bytes).into_owned(),
                        Err(_) => return Err(format!("{name}: 不是文本文件，暂不支持")),
                    }
                }
            };
            let (text, note) = truncate(text, file_chars);
            Ok(Processed {
                text_block: Some(file_block(&name, &text, note.as_deref())),
                meta: AttachmentMeta { kind: "file".into(), name, file: None, desc: None, note, call: None },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn truncates_long_text_with_note() {
        let (t, note) = truncate("一二三四五".into(), 3);
        assert_eq!(t, "一二三");
        assert!(note.unwrap().contains("共 5 字"));
        assert!(truncate("短".into(), 3).1.is_none());
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(read_saved("..\\config.json").is_err());
        assert!(read_saved("a/b.png").is_err());
    }

    #[test]
    fn extracts_docx_text() {
        let dir = std::env::temp_dir().join(format!("tp-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.docx");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(
            br#"<w:document><w:body><w:p><w:r><w:t>Hello &amp; </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t><w:tab/><w:t>line</w:t></w:r></w:p></w:body></w:document>"#,
        )
        .unwrap();
        zip.finish().unwrap();
        let text = extract_docx(&path).unwrap();
        assert_eq!(text, "Hello & world\nsecond\tline\n");
        let _ = std::fs::remove_dir_all(dir);
    }
}
