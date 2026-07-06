use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use crate::{RouteMount, ServeError};

#[derive(Debug, Clone)]
pub struct StaticFileService;

impl StaticFileService {
    pub fn plan(mount: &RouteMount, relative_path: &str) -> ServeOutcome {
        match resolve_under_root(&mount.local_root, relative_path) {
            Ok(target) => plan_resolved_path(mount, target),
            Err(error) => ServeOutcome::Forbidden {
                reason: error.to_string(),
            },
        }
    }
}

fn plan_resolved_path(mount: &RouteMount, target: PathBuf) -> ServeOutcome {
    match fs::metadata(&target) {
        Ok(metadata) if metadata.is_file() => {
            let content_type = content_type_for_path(&target).to_string();
            let render_mode = render_mode_for_path(mount, &target);
            ServeOutcome::File(ServeFilePlan {
                path: target,
                content_type,
                render_mode,
                len: metadata.len(),
                modified: metadata.modified().ok(),
            })
        }
        Ok(metadata) if metadata.is_dir() => {
            let index_path = target.join(&mount.index_file);
            if let Ok(index_metadata) = fs::metadata(&index_path) {
                // index_path is constructed locally and not validated by resolve_under_root,
                // so we must check it stays inside the root before serving it.
                if index_metadata.is_file() && is_inside_root(&mount.local_root, &index_path) {
                    return ServeOutcome::File(ServeFilePlan {
                        path: index_path.clone(),
                        content_type: content_type_for_path(&index_path).to_string(),
                        render_mode: render_mode_for_path(mount, &index_path),
                        len: index_metadata.len(),
                        modified: index_metadata.modified().ok(),
                    });
                }
            }

            match read_directory_listing(&target) {
                Ok(entries) => ServeOutcome::DirectoryListing {
                    path: target,
                    entries,
                },
                Err(error) => ServeOutcome::Forbidden {
                    reason: error.to_string(),
                },
            }
        }
        Ok(_) => ServeOutcome::Forbidden {
            reason: "path is not a file or directory".to_string(),
        },
        Err(_) if mount.spa => plan_spa_fallback(mount),
        Err(_) => ServeOutcome::NotFound { path: target },
    }
}

fn plan_spa_fallback(mount: &RouteMount) -> ServeOutcome {
    let index_path = mount.local_root.join(&mount.index_file);
    match fs::metadata(&index_path) {
        Ok(metadata) if metadata.is_file() && is_inside_root(&mount.local_root, &index_path) => {
            ServeOutcome::File(ServeFilePlan {
                path: index_path,
                content_type: content_type_for_path(&mount.local_root.join(&mount.index_file))
                    .to_string(),
                render_mode: render_mode_for_path(mount, &mount.local_root.join(&mount.index_file)),
                len: metadata.len(),
                modified: metadata.modified().ok(),
            })
        }
        _ => ServeOutcome::NotFound { path: index_path },
    }
}

fn resolve_under_root(root: &Path, relative_path: &str) -> Result<PathBuf, ServeError> {
    if relative_path.as_bytes().contains(&0) {
        return Err(ServeError::InvalidRoute(
            "request path contains null byte".to_string(),
        ));
    }

    let relative_path = percent_decode_path(relative_path)?;
    let root = root
        .canonicalize()
        .map_err(|_| ServeError::PathNotFound(root.display().to_string()))?;
    let mut target = root.clone();

    for component in Path::new(&relative_path).components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                return Err(ServeError::InvalidRoute(
                    "request path escapes mount root".to_string(),
                ));
            }
            Component::Normal(segment) => target.push(segment),
        }
    }

    if is_inside_root(&root, &target) {
        Ok(target)
    } else {
        Err(ServeError::InvalidRoute(
            "request path escapes mount root".to_string(),
        ))
    }
}

fn is_inside_root(root: &Path, target: &Path) -> bool {
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    let Ok(target) = target.canonicalize() else {
        return target.starts_with(&root);
    };
    target.starts_with(root)
}

fn percent_decode_path(value: &str) -> Result<String, ServeError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(ServeError::InvalidRoute(
                    "request path contains incomplete percent encoding".to_string(),
                ));
            }
            let high = decode_hex(bytes[index + 1])?;
            let low = decode_hex(bytes[index + 2])?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(decoded)
        .map_err(|_| ServeError::InvalidRoute("request path is not valid UTF-8".to_string()))
}

fn decode_hex(byte: u8) -> Result<u8, ServeError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ServeError::InvalidRoute(
            "request path contains invalid percent encoding".to_string(),
        )),
    }
}

fn read_directory_listing(path: &Path) -> Result<Vec<DirectoryEntry>, ServeError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).map_err(|err| ServeError::PathNotReadable(err.to_string()))? {
        let entry = entry.map_err(|err| ServeError::PathNotReadable(err.to_string()))?;
        let metadata = entry
            .metadata()
            .map_err(|err| ServeError::PathNotReadable(err.to_string()))?;
        entries.push(DirectoryEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            kind: if metadata.is_dir() {
                DirectoryEntryKind::Directory
            } else {
                DirectoryEntryKind::File
            },
            len: metadata.len(),
            modified: metadata.modified().ok(),
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("gif") => "image/gif",
        Some("htm" | "html") => "text/html; charset=utf-8",
        Some("jpeg" | "jpg") => "image/jpeg",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("md" | "markdown") => "text/markdown; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn render_mode_for_path(mount: &RouteMount, path: &Path) -> RenderMode {
    let extension = path.extension().and_then(|extension| extension.to_str());
    match extension {
        Some("md" | "markdown") if mount.render.markdown => RenderMode::Markdown,
        Some(extension) if mount.render.code_highlight && is_code_extension(extension) => {
            RenderMode::CodeHighlight
        }
        _ => RenderMode::Raw,
    }
}

fn is_code_extension(extension: &str) -> bool {
    matches!(
        extension,
        "c" | "cc"
            | "cpp"
            | "css"
            | "go"
            | "h"
            | "hpp"
            | "html"
            | "java"
            | "js"
            | "jsx"
            | "json"
            | "mjs"
            | "py"
            | "rs"
            | "sh"
            | "ts"
            | "tsx"
            | "toml"
            | "yaml"
            | "yml"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeOutcome {
    File(ServeFilePlan),
    DirectoryListing {
        path: PathBuf,
        entries: Vec<DirectoryEntry>,
    },
    NotFound {
        path: PathBuf,
    },
    Forbidden {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeFilePlan {
    pub path: PathBuf,
    pub content_type: String,
    pub render_mode: RenderMode,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Raw,
    Markdown,
    CodeHighlight,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: DirectoryEntryKind,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryEntryKind {
    File,
    Directory,
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use tempfile::TempDir;

    use super::*;
    use crate::{ListenerKey, MountId, NormalizedRoute};

    fn mount(root: &Path, spa: bool) -> RouteMount {
        RouteMount {
            id: MountId::new(),
            listener: ListenerKey {
                bind_addr: IpAddr::from([127, 0, 0, 1]),
                port: 8088,
            },
            route: "/app".parse::<NormalizedRoute>().unwrap(),
            local_root: root.to_path_buf(),
            index_file: "index.html".to_string(),
            spa,
            render: Default::default(),
            readonly: true,
            expires_at: None,
            display_name: None,
        }
    }

    #[test]
    fn serves_regular_file() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("hello.txt"), "hello").unwrap();
        let mount = mount(temp.path(), false);

        let outcome = StaticFileService::plan(&mount, "hello.txt");

        let ServeOutcome::File(file) = outcome else {
            panic!("expected file");
        };
        assert_eq!(file.len, 5);
        assert_eq!(file.content_type, "text/plain; charset=utf-8");
    }

    #[test]
    fn serves_index_before_directory_listing() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("index.html"), "<h1>app</h1>").unwrap();
        fs::write(temp.path().join("other.txt"), "other").unwrap();
        let mount = mount(temp.path(), false);

        let outcome = StaticFileService::plan(&mount, "");

        let ServeOutcome::File(file) = outcome else {
            panic!("expected index file");
        };
        assert_eq!(file.path, temp.path().join("index.html"));
        assert_eq!(file.content_type, "text/html; charset=utf-8");
    }

    #[test]
    fn falls_back_to_directory_listing_without_index() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("a.txt"), "a").unwrap();
        fs::create_dir(temp.path().join("nested")).unwrap();
        let mount = mount(temp.path(), false);

        let outcome = StaticFileService::plan(&mount, "");

        let ServeOutcome::DirectoryListing { entries, .. } = outcome else {
            panic!("expected directory listing");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[1].name, "nested");
        assert_eq!(entries[1].kind, DirectoryEntryKind::Directory);
    }

    #[test]
    fn supports_spa_fallback() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("index.html"), "<main></main>").unwrap();
        let mount = mount(temp.path(), true);

        let outcome = StaticFileService::plan(&mount, "missing/route");

        let ServeOutcome::File(file) = outcome else {
            panic!("expected spa index file");
        };
        assert_eq!(file.path, temp.path().join("index.html"));
    }

    #[test]
    fn returns_not_found_without_spa_fallback() {
        let temp = TempDir::new().unwrap();
        let mount = mount(temp.path(), false);

        let outcome = StaticFileService::plan(&mount, "missing.txt");

        assert!(matches!(outcome, ServeOutcome::NotFound { .. }));
    }

    #[test]
    fn rejects_parent_dir_traversal() {
        let temp = TempDir::new().unwrap();
        let mount = mount(temp.path(), false);

        let outcome = StaticFileService::plan(&mount, "../secret.txt");

        assert!(matches!(outcome, ServeOutcome::Forbidden { .. }));
    }

    #[test]
    fn rejects_percent_decoded_parent_dir_traversal() {
        let temp = TempDir::new().unwrap();
        let mount = mount(temp.path(), false);

        let outcome = StaticFileService::plan(&mount, "%2e%2e/secret.txt");

        assert!(matches!(outcome, ServeOutcome::Forbidden { .. }));
    }

    #[test]
    fn decodes_percent_encoded_file_names() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("hello world.txt"), "hello").unwrap();
        let mount = mount(temp.path(), false);

        let outcome = StaticFileService::plan(&mount, "hello%20world.txt");

        let ServeOutcome::File(file) = outcome else {
            panic!("expected decoded file path");
        };
        assert_eq!(file.path, temp.path().join("hello world.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            root.path().join("secret-link"),
        )
        .unwrap();
        let mount = mount(root.path(), false);

        let outcome = StaticFileService::plan(&mount, "secret-link");

        assert!(matches!(outcome, ServeOutcome::Forbidden { .. }));
    }

    #[test]
    fn supports_custom_index_file() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("home.html"), "home").unwrap();
        let mut mount = mount(temp.path(), false);
        mount.index_file = "home.html".to_string();

        let outcome = StaticFileService::plan(&mount, "");

        let ServeOutcome::File(file) = outcome else {
            panic!("expected custom index file");
        };
        assert_eq!(file.path, temp.path().join("home.html"));
    }

    #[test]
    fn marks_markdown_for_rendering_when_enabled() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("README.md"), "# Hello").unwrap();
        let mut mount = mount(temp.path(), false);
        mount.render.markdown = true;

        let outcome = StaticFileService::plan(&mount, "README.md");
        let ServeOutcome::File(file) = outcome else {
            panic!("expected file");
        };

        assert_eq!(file.content_type, "text/markdown; charset=utf-8");
        assert_eq!(file.render_mode, RenderMode::Markdown);
    }

    #[test]
    fn leaves_markdown_raw_when_rendering_is_disabled() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("README.md"), "# Hello").unwrap();
        let mount = mount(temp.path(), false);

        let outcome = StaticFileService::plan(&mount, "README.md");
        let ServeOutcome::File(file) = outcome else {
            panic!("expected file");
        };

        assert_eq!(file.render_mode, RenderMode::Raw);
    }

    #[test]
    fn marks_code_for_highlighting_when_enabled() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("app.js"), "const value = 1;").unwrap();
        let mut mount = mount(temp.path(), false);
        mount.render.code_highlight = true;

        let outcome = StaticFileService::plan(&mount, "app.js");
        let ServeOutcome::File(file) = outcome else {
            panic!("expected file");
        };

        assert_eq!(file.render_mode, RenderMode::CodeHighlight);
    }
}
