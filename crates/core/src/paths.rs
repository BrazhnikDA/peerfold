use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel.len() > 4096 || rel.starts_with('/') || rel.starts_with('\\') {
        return Err(Error::msg("недопустимый путь"));
    }
    let mut out = root.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('\\')
            || part.contains('\0')
        {
            return Err(Error::msg("недопустимый путь"));
        }
        out.push(part);
    }
    Ok(out)
}

pub fn friendly_path(path: &Path) -> PathBuf {
    PathBuf::from(strip_extended_prefix(&path.to_string_lossy()))
}

pub fn strip_extended_prefix(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    text.to_string()
}

pub fn is_transient(rel: &str) -> bool {
    rel.split('/').any(|p| p.ends_with(".peerfold-partial"))
}

/// User ignore hook. Always false until `.syncignore` is implemented.
pub fn is_user_ignored(_rel: &str) -> bool {
    false
}

pub fn conflict_rel(rel: &str, device_id: &str, unix_secs: i64, n: u32) -> String {
    let short: String = device_id.chars().take(6).collect();
    let (parent, name) = match rel.rfind('/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => ("", rel),
    };
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let extra = if n == 0 {
        String::new()
    } else {
        format!("-{n}")
    };
    let file = format!("{stem}.conflict-{short}-{unix_secs}{extra}{ext}");
    if parent.is_empty() {
        file
    } else {
        format!("{parent}/{file}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_segments() {
        assert!(safe_join(Path::new("/tmp/share"), "../x").is_err());
        assert!(safe_join(Path::new("/tmp/share"), "a/../../b").is_err());
        assert!(safe_join(Path::new("/tmp/share"), "/etc/passwd").is_err());
    }

    #[test]
    fn joins_nested() {
        let path = safe_join(Path::new("/tmp/share"), "src/main.rs").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/share/src/main.rs"));
    }

    #[test]
    fn windows_extended_path_is_shown_normally() {
        assert_eq!(
            strip_extended_prefix(r"\\?\C:\Users\Kate\Documents\проект"),
            r"C:\Users\Kate\Documents\проект"
        );
        assert_eq!(
            strip_extended_prefix(r"\\?\UNC\nas\share\проект"),
            r"\\nas\share\проект"
        );
        assert_eq!(strip_extended_prefix("/home/user/проект"), "/home/user/проект");
    }

    #[test]
    fn conflict_name_keeps_extension() {
        let name = conflict_rel("docs/note.txt", "abcdef1234", 10, 0);
        assert_eq!(name, "docs/note.conflict-abcdef-10.txt");
    }
}
