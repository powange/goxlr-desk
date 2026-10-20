use anyhow::{Result, anyhow};
use std::fs;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes a file so that it's never left missing or half written: the content goes to a new
/// temporary file next to it, which is synced and then renamed over the target. The rename
/// replaces the target in one step (on Windows too), so a crash leaves either the old file or
/// the new one. Each call uses its own temporary file, so saves never share one.
pub fn atomic_write<F>(path: impl AsRef<Path>, write: F) -> Result<()>
where
    F: FnOnce(&File) -> Result<()>,
{
    let path = path.as_ref();
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("Invalid file path: {:?}", path))?;
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };

    let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = directory.join(format!(
        ".{}.{}-{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        count
    ));

    let result: Result<()> = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        write(&file)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
        return result;
    }

    // Make sure the rename itself is on disk.
    #[cfg(unix)]
    if let Ok(dir) = File::open(directory) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn replaces_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("goxlr-atomic-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        atomic_write(&path, |mut f| Ok(f.write_all(b"first")?)).unwrap();
        atomic_write(&path, |mut f| Ok(f.write_all(b"second")?)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");

        // A failing write leaves the previous content, and no temporary file behind.
        let failed = atomic_write(&path, |mut f| {
            f.write_all(b"partial")?;
            Err(anyhow!("disk full"))
        });
        assert!(failed.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);

        fs::remove_dir_all(&dir).unwrap();
    }
}
