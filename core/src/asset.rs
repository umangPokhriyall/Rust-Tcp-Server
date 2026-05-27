//! In-memory static-asset cache. Files are loaded into memory ONCE at boot and
//! served from RAM thereafter — no per-request disk I/O (a benchmark-correctness
//! requirement).

pub struct Asset {
    pub bytes: std::sync::Arc<[u8]>,
    pub content_type: &'static str,
}

pub struct AssetCache {
    // HashMap<String, Asset>, populated in Session C.
}

impl AssetCache {
    /// Load every file under `dir` into memory. Called once, at startup.
    pub fn load_dir(_dir: &std::path::Path) -> std::io::Result<Self> {
        todo!()
    }

    pub fn get(&self, _name: &str) -> Option<&Asset> {
        todo!()
    }
}
