//! In-memory static-asset cache. Files are loaded into memory ONCE at boot and
//! served from RAM thereafter — no per-request disk I/O (a benchmark-correctness
//! requirement).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub struct Asset {
    pub bytes: Arc<[u8]>,
    pub content_type: &'static str,
}

/// Static files loaded into memory once at boot, served from RAM thereafter.
#[derive(Default)]
pub struct AssetCache {
    /// Keyed by path relative to the load directory, using `/` separators
    /// (e.g. `index.html`, `static/style.css`).
    map: HashMap<String, Asset>,
}

impl AssetCache {
    /// Load every file under `dir` (recursively) into memory. Called once, at
    /// startup.
    pub fn load_dir(dir: &Path) -> std::io::Result<Self> {
        let mut map = HashMap::new();
        load_into(dir, dir, &mut map)?;
        Ok(AssetCache { map })
    }

    pub fn get(&self, name: &str) -> Option<&Asset> {
        self.map.get(name)
    }
}

/// Recursively walk `dir`, inserting each file under its path relative to
/// `root` (with `/` separators).
fn load_into(root: &Path, dir: &Path, map: &mut HashMap<String, Asset>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            load_into(root, &path, map)?;
        } else {
            let bytes = std::fs::read(&path)?;
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let key = rel.to_string_lossy().replace('\\', "/");
            map.insert(
                key,
                Asset {
                    bytes: Arc::from(bytes),
                    content_type: content_type_for(&path),
                },
            );
        }
    }
    Ok(())
}

/// Best-effort MIME type from a file extension. Returns a `'static` literal so
/// `Asset::content_type` need not allocate.
fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_by_extension() {
        assert_eq!(content_type_for(Path::new("a.html")), "text/html; charset=utf-8");
        assert_eq!(content_type_for(Path::new("a.css")), "text/css; charset=utf-8");
        assert_eq!(content_type_for(Path::new("a.js")), "text/javascript; charset=utf-8");
        assert_eq!(content_type_for(Path::new("a.png")), "image/png");
        assert_eq!(content_type_for(Path::new("a.unknownext")), "application/octet-stream");
        assert_eq!(content_type_for(Path::new("noext")), "application/octet-stream");
    }

    #[test]
    fn loads_files_recursively_with_content_types() {
        let base = std::env::temp_dir().join(format!("core_asset_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("static")).unwrap();
        std::fs::write(base.join("index.html"), b"<h1>hi</h1>").unwrap();
        std::fs::write(base.join("static/style.css"), b"body{}").unwrap();

        let cache = AssetCache::load_dir(&base).unwrap();

        let index = cache.get("index.html").expect("index.html present");
        assert_eq!(&index.bytes[..], b"<h1>hi</h1>");
        assert_eq!(index.content_type, "text/html; charset=utf-8");

        let css = cache.get("static/style.css").expect("static/style.css present");
        assert_eq!(&css.bytes[..], b"body{}");
        assert_eq!(css.content_type, "text/css; charset=utf-8");

        assert!(cache.get("missing").is_none());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn empty_cache_returns_none() {
        let cache = AssetCache::default();
        assert!(cache.get("anything").is_none());
    }
}
