//! Freedesktop Icon Theme Specification implementation.
//!
//! Implements theme lookup with inheritance chains, mandatory `hicolor` fallback,
//! SVG rasterization via resvg, and an LRU cache for decoded bitmaps.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A rasterized icon as RGBA pixel data.
#[derive(Clone)]
pub struct IconImage {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Key for the icon cache.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct CacheKey {
    theme: String,
    name: String,
    size: u32,
    scale: u32,
}

/// A parsed icon theme directory entry from index.theme.
#[derive(Debug, Clone)]
struct ThemeDirectory {
    path: String,
    size: u32,
    scale: u32,
    #[allow(dead_code)]
    context: String,
    dir_type: DirectoryType,
    max_size: u32,
    min_size: u32,
    threshold: u32,
}

#[derive(Debug, Clone, PartialEq)]
enum DirectoryType {
    Fixed,
    Scalable,
    Threshold,
}

/// A parsed icon theme (from index.theme).
#[derive(Debug, Clone)]
struct ThemeInfo {
    #[allow(dead_code)]
    name: String,
    inherits: Vec<String>,
    directories: Vec<ThemeDirectory>,
}

/// Icon manager implementing the freedesktop Icon Theme Specification.
pub struct IconManager {
    /// Base directories to search for icon themes.
    base_dirs: Vec<PathBuf>,
    /// Parsed theme info cache (theme name -> ThemeInfo).
    themes: HashMap<String, ThemeInfo>,
    /// LRU icon cache.
    cache: Arc<Mutex<LruCache>>,
    /// Maximum cache entries.
    #[allow(dead_code)]
    max_cache_size: usize,
}

/// Simple LRU cache backed by a Vec (sufficient for panel icon counts).
struct LruCache {
    entries: Vec<(CacheKey, IconImage)>,
    max_size: usize,
}

impl LruCache {
    fn new(max_size: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_size,
        }
    }

    fn get(&mut self, key: &CacheKey) -> Option<IconImage> {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
            // Move to end (most recently used)
            let entry = self.entries.remove(pos);
            let img = entry.1.clone();
            self.entries.push(entry);
            Some(img)
        } else {
            None
        }
    }

    fn insert(&mut self, key: CacheKey, image: IconImage) {
        // Remove if already present
        self.entries.retain(|(k, _)| k != &key);
        // Evict oldest if full
        while self.entries.len() >= self.max_size {
            self.entries.remove(0);
        }
        self.entries.push((key, image));
    }
}

impl IconManager {
    /// Create a new icon manager with standard freedesktop base directories.
    pub fn new(max_cache_size: usize) -> Self {
        let mut base_dirs = Vec::new();

        // $HOME/.icons
        if let Some(home) = dirs::home_dir() {
            base_dirs.push(home.join(".icons"));
        }

        // $XDG_DATA_DIRS/icons
        if let Ok(data_dirs) = std::env::var("XDG_DATA_DIRS") {
            for dir in data_dirs.split(':') {
                if !dir.is_empty() {
                    base_dirs.push(PathBuf::from(dir).join("icons"));
                }
            }
        } else {
            base_dirs.push(PathBuf::from("/usr/local/share/icons"));
            base_dirs.push(PathBuf::from("/usr/share/icons"));
        }

        // $XDG_DATA_HOME/icons
        if let Some(data_home) = dirs::data_dir() {
            base_dirs.push(data_home.join("icons"));
        }

        // /usr/share/pixmaps as final fallback
        base_dirs.push(PathBuf::from("/usr/share/pixmaps"));

        Self {
            base_dirs,
            themes: HashMap::new(),
            cache: Arc::new(Mutex::new(LruCache::new(max_cache_size))),
            max_cache_size,
        }
    }

    /// Look up and rasterize an icon by name, theme, size, and scale.
    /// Follows the freedesktop lookup algorithm:
    /// 1. Search the specified theme
    /// 2. Search inherited themes
    /// 3. Fallback to hicolor
    /// 4. Fallback to unthemed lookup in base dirs
    pub fn lookup(
        &mut self,
        icon_name: &str,
        theme_name: &str,
        size: u32,
        scale: u32,
    ) -> Option<IconImage> {
        let key = CacheKey {
            theme: theme_name.to_string(),
            name: icon_name.to_string(),
            size,
            scale,
        };

        // Check cache
        if let Some(img) = self.cache.lock().unwrap().get(&key) {
            return Some(img);
        }

        // Perform lookup
        let result = self.lookup_uncached(icon_name, theme_name, size, scale);

        // Cache result
        if let Some(ref img) = result {
            self.cache.lock().unwrap().insert(key, img.clone());
        }

        result
    }

    fn lookup_uncached(
        &mut self,
        icon_name: &str,
        theme_name: &str,
        size: u32,
        scale: u32,
    ) -> Option<IconImage> {
        // Step 1: Look in the named theme
        if let Some(path) = self.find_icon_in_theme(icon_name, theme_name, size, scale) {
            return self.load_and_rasterize(&path, size, scale);
        }

        // Step 2: Look in inherited themes (recursively)
        let inherits = self.get_theme_inherits(theme_name);
        for parent in inherits {
            if let Some(path) = self.find_icon_in_theme(icon_name, &parent, size, scale) {
                return self.load_and_rasterize(&path, size, scale);
            }
        }

        // Step 3: Fallback to hicolor (mandatory per spec)
        if theme_name != "hicolor" {
            if let Some(path) = self.find_icon_in_theme(icon_name, "hicolor", size, scale) {
                return self.load_and_rasterize(&path, size, scale);
            }
        }

        // Step 4: Unthemed lookup (search base dirs directly)
        self.find_unthemed_icon(icon_name, size, scale)
    }

    /// Parse an index.theme file and cache the result.
    fn ensure_theme_loaded(&mut self, theme_name: &str) {
        if self.themes.contains_key(theme_name) {
            return;
        }

        for base in &self.base_dirs.clone() {
            let index_path = base.join(theme_name).join("index.theme");
            if index_path.exists() {
                if let Ok(info) = parse_index_theme(&index_path, theme_name) {
                    self.themes.insert(theme_name.to_string(), info);
                    return;
                }
            }
        }
    }

    /// Get the inheritance chain for a theme.
    fn get_theme_inherits(&mut self, theme_name: &str) -> Vec<String> {
        self.ensure_theme_loaded(theme_name);
        self.themes
            .get(theme_name)
            .map(|t| t.inherits.clone())
            .unwrap_or_default()
    }

    /// Find an icon file in a specific theme, following the freedesktop algorithm.
    fn find_icon_in_theme(
        &mut self,
        icon_name: &str,
        theme_name: &str,
        size: u32,
        scale: u32,
    ) -> Option<PathBuf> {
        self.ensure_theme_loaded(theme_name);

        let theme = self.themes.get(theme_name)?;
        let extensions = ["svg", "png", "xpm"];

        // First pass: find exact size match
        for dir in &theme.directories {
            if directory_matches_size(dir, size, scale) {
                for base in &self.base_dirs {
                    let theme_dir = base.join(theme_name).join(&dir.path);
                    for ext in &extensions {
                        let path = theme_dir.join(format!("{icon_name}.{ext}"));
                        if path.exists() {
                            return Some(path);
                        }
                    }
                }
            }
        }

        // Second pass: find closest size match
        let mut best_path: Option<PathBuf> = None;
        let mut best_distance = u32::MAX;

        for dir in &theme.directories {
            let dist = directory_size_distance(dir, size, scale);
            if dist < best_distance {
                for base in &self.base_dirs {
                    let theme_dir = base.join(theme_name).join(&dir.path);
                    for ext in &extensions {
                        let path = theme_dir.join(format!("{icon_name}.{ext}"));
                        if path.exists() {
                            best_distance = dist;
                            best_path = Some(path);
                        }
                    }
                }
            }
        }

        best_path
    }

    /// Find an icon in unthemed directories (e.g., /usr/share/pixmaps).
    fn find_unthemed_icon(&self, icon_name: &str, size: u32, scale: u32) -> Option<IconImage> {
        let extensions = ["svg", "png", "xpm"];
        for base in &self.base_dirs {
            for ext in &extensions {
                let path = base.join(format!("{icon_name}.{ext}"));
                if path.exists() {
                    return self.load_and_rasterize(&path, size, scale);
                }
            }
        }
        None
    }

    /// Load an icon file and rasterize it to the target size.
    fn load_and_rasterize(&self, path: &Path, size: u32, scale: u32) -> Option<IconImage> {
        let target_size = size * scale;

        let ext = path.extension()?.to_str()?.to_lowercase();
        match ext.as_str() {
            "svg" => rasterize_svg(path, target_size),
            "png" => load_png(path, target_size),
            _ => {
                tracing::debug!("unsupported icon format: {}", ext);
                None
            }
        }
    }

    /// Prewarm the cache with commonly used icons.
    pub fn prewarm(&mut self, icon_names: &[&str], theme_name: &str, size: u32, scale: u32) {
        for name in icon_names {
            let _ = self.lookup(name, theme_name, size, scale);
        }
    }

    /// Clear the icon cache.
    pub fn clear_cache(&self) {
        let mut cache = self.cache.lock().unwrap();
        cache.entries.clear();
    }

    /// Number of cached icons.
    pub fn cache_size(&self) -> usize {
        self.cache.lock().unwrap().entries.len()
    }
}

/// Parse an index.theme file per the freedesktop Icon Theme Specification.
fn parse_index_theme(path: &Path, theme_name: &str) -> Result<ThemeInfo> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    let mut inherits = Vec::new();
    let mut dir_names = Vec::new();
    let mut directories = Vec::new();
    let mut current_section = String::new();
    let mut section_values: HashMap<String, String> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            // Save previous section if it was a directory
            if !current_section.is_empty()
                && current_section != "Icon Theme"
                && dir_names.contains(&current_section)
            {
                if let Some(dir) = parse_directory_section(&current_section, &section_values) {
                    directories.push(dir);
                }
            }
            current_section = line[1..line.len() - 1].to_string();
            section_values.clear();
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            if current_section == "Icon Theme" {
                match key {
                    "Inherits" => {
                        inherits = value
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                    "Directories" | "ScaledDirectories" => {
                        for d in value.split(',') {
                            let d = d.trim().to_string();
                            if !d.is_empty() && !dir_names.contains(&d) {
                                dir_names.push(d);
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                section_values.insert(key.to_string(), value.to_string());
            }
        }
    }

    // Process the last section
    if !current_section.is_empty()
        && current_section != "Icon Theme"
        && dir_names.contains(&current_section)
    {
        if let Some(dir) = parse_directory_section(&current_section, &section_values) {
            directories.push(dir);
        }
    }

    Ok(ThemeInfo {
        name: theme_name.to_string(),
        inherits,
        directories,
    })
}

/// Parse a directory section from index.theme.
fn parse_directory_section(path: &str, values: &HashMap<String, String>) -> Option<ThemeDirectory> {
    let size = values.get("Size")?.parse::<u32>().ok()?;
    let scale = values
        .get("Scale")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let context = values.get("Context").cloned().unwrap_or_default();
    let dir_type = match values.get("Type").map(|s| s.as_str()) {
        Some("Scalable") => DirectoryType::Scalable,
        Some("Threshold") => DirectoryType::Threshold,
        _ => DirectoryType::Fixed,
    };
    let max_size = values
        .get("MaxSize")
        .and_then(|v| v.parse().ok())
        .unwrap_or(size);
    let min_size = values
        .get("MinSize")
        .and_then(|v| v.parse().ok())
        .unwrap_or(size);
    let threshold = values
        .get("Threshold")
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);

    Some(ThemeDirectory {
        path: path.to_string(),
        size,
        scale,
        context,
        dir_type,
        max_size,
        min_size,
        threshold,
    })
}

/// Check if a directory entry matches the requested size per spec.
fn directory_matches_size(dir: &ThemeDirectory, size: u32, scale: u32) -> bool {
    if dir.scale != scale {
        return false;
    }
    match dir.dir_type {
        DirectoryType::Fixed => dir.size == size,
        DirectoryType::Scalable => size >= dir.min_size && size <= dir.max_size,
        DirectoryType::Threshold => {
            size >= dir.size.saturating_sub(dir.threshold) && size <= dir.size + dir.threshold
        }
    }
}

/// Calculate the distance between a directory entry and the requested size.
fn directory_size_distance(dir: &ThemeDirectory, size: u32, scale: u32) -> u32 {
    let scale_penalty = if dir.scale != scale { 1000 } else { 0 };
    let size_dist = match dir.dir_type {
        DirectoryType::Fixed => {
            (dir.size as i64 * dir.scale as i64 - size as i64 * scale as i64).unsigned_abs() as u32
        }
        DirectoryType::Scalable => {
            let ds = dir.scale as i64;
            let s = size as i64 * scale as i64;
            if s < dir.min_size as i64 * ds {
                (dir.min_size as i64 * ds - s).unsigned_abs() as u32
            } else if s > dir.max_size as i64 * ds {
                (s - dir.max_size as i64 * ds).unsigned_abs() as u32
            } else {
                0
            }
        }
        DirectoryType::Threshold => {
            let ds = dir.scale as i64;
            let s = size as i64 * scale as i64;
            let low = (dir.size as i64 - dir.threshold as i64) * ds;
            let high = (dir.size as i64 + dir.threshold as i64) * ds;
            if s < low {
                (low - s).unsigned_abs() as u32
            } else if s > high {
                (s - high).unsigned_abs() as u32
            } else {
                0
            }
        }
    };
    size_dist + scale_penalty
}

/// Rasterize an SVG file to the target pixel size using resvg.
fn rasterize_svg(path: &Path, target_size: u32) -> Option<IconImage> {
    let data = std::fs::read(path).ok()?;
    let opts = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(&data, &opts).ok()?;

    let original_size = tree.size();
    let scale_x = target_size as f32 / original_size.width();
    let scale_y = target_size as f32 / original_size.height();
    let scale = scale_x.min(scale_y);
    let final_w = (original_size.width() * scale).ceil() as u32;
    let final_h = (original_size.height() * scale).ceil() as u32;

    let mut pixmap = tiny_skia::Pixmap::new(final_w, final_h)?;
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Some(IconImage {
        data: pixmap.data().to_vec(),
        width: final_w,
        height: final_h,
    })
}

/// Load a PNG file and optionally resize to target size.
fn load_png(path: &Path, target_size: u32) -> Option<IconImage> {
    let data = std::fs::read(path).ok()?;
    let pixmap = tiny_skia::Pixmap::decode_png(&data).ok()?;

    if pixmap.width() == target_size && pixmap.height() == target_size {
        return Some(IconImage {
            data: pixmap.data().to_vec(),
            width: pixmap.width(),
            height: pixmap.height(),
        });
    }

    // Simple nearest-neighbor resize
    let src_w = pixmap.width();
    let src_h = pixmap.height();
    let scale_x = src_w as f32 / target_size as f32;
    let scale_y = src_h as f32 / target_size as f32;
    let scale = scale_x.max(scale_y);
    let dst_w = (src_w as f32 / scale).ceil() as u32;
    let dst_h = (src_h as f32 / scale).ceil() as u32;

    let mut out = vec![0u8; (dst_w * dst_h * 4) as usize];
    let src_data = pixmap.data();

    for y in 0..dst_h {
        for x in 0..dst_w {
            let sx = ((x as f32 * scale) as u32).min(src_w - 1);
            let sy = ((y as f32 * scale) as u32).min(src_h - 1);
            let si = ((sy * src_w + sx) * 4) as usize;
            let di = ((y * dst_w + x) * 4) as usize;
            out[di..di + 4].copy_from_slice(&src_data[si..si + 4]);
        }
    }

    Some(IconImage {
        data: out,
        width: dst_w,
        height: dst_h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_directory_matches_fixed() {
        let dir = ThemeDirectory {
            path: "48x48/apps".into(),
            size: 48,
            scale: 1,
            context: "Applications".into(),
            dir_type: DirectoryType::Fixed,
            max_size: 48,
            min_size: 48,
            threshold: 2,
        };
        assert!(directory_matches_size(&dir, 48, 1));
        assert!(!directory_matches_size(&dir, 32, 1));
        assert!(!directory_matches_size(&dir, 48, 2));
    }

    #[test]
    fn test_directory_matches_scalable() {
        let dir = ThemeDirectory {
            path: "scalable/apps".into(),
            size: 48,
            scale: 1,
            context: "Applications".into(),
            dir_type: DirectoryType::Scalable,
            max_size: 256,
            min_size: 16,
            threshold: 2,
        };
        assert!(directory_matches_size(&dir, 48, 1));
        assert!(directory_matches_size(&dir, 16, 1));
        assert!(directory_matches_size(&dir, 256, 1));
        assert!(!directory_matches_size(&dir, 8, 1));
        assert!(!directory_matches_size(&dir, 512, 1));
    }

    #[test]
    fn test_directory_matches_threshold() {
        let dir = ThemeDirectory {
            path: "48x48/apps".into(),
            size: 48,
            scale: 1,
            context: "Applications".into(),
            dir_type: DirectoryType::Threshold,
            max_size: 48,
            min_size: 48,
            threshold: 2,
        };
        assert!(directory_matches_size(&dir, 48, 1));
        assert!(directory_matches_size(&dir, 46, 1));
        assert!(directory_matches_size(&dir, 50, 1));
        assert!(!directory_matches_size(&dir, 45, 1));
    }

    #[test]
    fn test_lru_cache() {
        let mut cache = LruCache::new(2);
        let k1 = CacheKey {
            theme: "t".into(),
            name: "a".into(),
            size: 16,
            scale: 1,
        };
        let k2 = CacheKey {
            theme: "t".into(),
            name: "b".into(),
            size: 16,
            scale: 1,
        };
        let k3 = CacheKey {
            theme: "t".into(),
            name: "c".into(),
            size: 16,
            scale: 1,
        };
        let img = IconImage {
            data: vec![0; 4],
            width: 1,
            height: 1,
        };

        cache.insert(k1.clone(), img.clone());
        cache.insert(k2.clone(), img.clone());
        assert!(cache.get(&k1).is_some());
        assert!(cache.get(&k2).is_some());

        // Insert k3 should evict k1 (k2 was more recently accessed)
        cache.insert(k3.clone(), img.clone());
        assert!(cache.get(&k1).is_none());
        assert!(cache.get(&k2).is_some());
        assert!(cache.get(&k3).is_some());
    }

    #[test]
    fn test_icon_manager_creation() {
        let manager = IconManager::new(100);
        assert!(!manager.base_dirs.is_empty());
        assert_eq!(manager.cache_size(), 0);
    }
}
