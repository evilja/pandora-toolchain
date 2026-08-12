use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use tokio::fs::{File, OpenOptions, create_dir_all, symlink_metadata};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};

use super::error::{Result, TorrentError};
use super::metainfo::Metainfo;

const MAX_OPEN_FILES: usize = 64;

struct StoredFile {
    file: Option<File>,
    index: u64,
    path: PathBuf,
    offset: u64,
    length: u64,
    last_used: u64,
}

pub(crate) struct Storage {
    files: Vec<StoredFile>,
    clock: u64,
}

impl Storage {
    pub(crate) async fn create(
        root: &Path,
        metainfo: &Metainfo,
        selected: Option<&HashSet<u64>>,
    ) -> Result<Self> {
        create_dir_all(root).await?;
        reject_symlink(root).await?;
        let mut files = Vec::new();
        for torrent_file in &metainfo.files {
            if selected.is_some_and(|selection| !selection.contains(&torrent_file.index)) {
                continue;
            }
            let path = safe_join(root, &torrent_file.path)?;
            if let Some(parent) = path.parent() {
                create_safe_directories(root, parent).await?;
            }
            if symlink_metadata(&path)
                .await
                .ok()
                .is_some_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(TorrentError::metainfo(format!(
                    "refusing to overwrite symlink {}",
                    path.display()
                )));
            }
            let file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&path)
                .await?;
            file.set_len(torrent_file.length).await?;
            files.push(StoredFile {
                file: None,
                index: torrent_file.index,
                path,
                offset: torrent_file.offset,
                length: torrent_file.length,
                last_used: 0,
            });
        }
        Ok(Self { files, clock: 0 })
    }

    pub(crate) async fn write_piece(&mut self, piece_offset: u64, data: &[u8]) -> Result<()> {
        let piece_end = piece_offset
            .checked_add(
                u64::try_from(data.len())
                    .map_err(|_| TorrentError::metainfo("piece data length cannot fit in u64"))?,
            )
            .ok_or_else(|| TorrentError::metainfo("piece byte range overflow"))?;
        for index in 0..self.files.len() {
            let target = &self.files[index];
            let file_end = target
                .offset
                .checked_add(target.length)
                .ok_or_else(|| TorrentError::metainfo("file byte range overflow"))?;
            let overlap_start = piece_offset.max(target.offset);
            let overlap_end = piece_end.min(file_end);
            if overlap_start >= overlap_end {
                continue;
            }
            let data_start = usize::try_from(overlap_start - piece_offset)
                .map_err(|_| TorrentError::metainfo("piece slice offset overflow"))?;
            let data_end = usize::try_from(overlap_end - piece_offset)
                .map_err(|_| TorrentError::metainfo("piece slice end overflow"))?;
            let file_offset = overlap_start - target.offset;
            self.ensure_open(index).await?;
            let target = &mut self.files[index];
            let file = target
                .file
                .as_mut()
                .ok_or_else(|| TorrentError::metainfo("storage file did not open"))?;
            file.seek(SeekFrom::Start(file_offset)).await?;
            file.write_all(&data[data_start..data_end]).await?;
        }
        Ok(())
    }

    // A batch download hands a finished file to the encoder while its siblings are still
    // downloading, so that file's bytes have to be on disk before the completion is announced.
    pub(crate) async fn flush_file(&mut self, index: u64) -> Result<()> {
        let Some(position) = self.files.iter().position(|file| file.index == index) else {
            return Ok(());
        };
        if let Some(file) = self.files[position].file.as_mut() {
            file.flush().await?;
            file.sync_data().await?;
        }
        Ok(())
    }

    pub(crate) async fn finish(mut self) -> Result<()> {
        for target in &mut self.files {
            if let Some(mut file) = target.file.take() {
                file.flush().await?;
                file.sync_data().await?;
            }
        }
        Ok(())
    }

    async fn ensure_open(&mut self, index: usize) -> Result<()> {
        self.clock = self.clock.wrapping_add(1);
        if self.files[index].file.is_none() {
            let open_count = self.files.iter().filter(|file| file.file.is_some()).count();
            if open_count >= MAX_OPEN_FILES {
                if let Some(evict) = self
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, file)| file.file.is_some())
                    .min_by_key(|(_, file)| file.last_used)
                    .map(|(index, _)| index)
                {
                    if let Some(mut file) = self.files[evict].file.take() {
                        file.flush().await?;
                    }
                }
            }
            let path = self.files[index].path.clone();
            let file = OpenOptions::new().read(true).write(true).open(path).await?;
            self.files[index].file = Some(file);
        }
        self.files[index].last_used = self.clock;
        Ok(())
    }
}

fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(TorrentError::metainfo(format!(
            "unsafe output path {}",
            relative.display()
        )));
    }
    Ok(root.join(relative))
}

async fn create_safe_directories(root: &Path, target: &Path) -> Result<()> {
    let relative = target.strip_prefix(root).map_err(|_| {
        TorrentError::metainfo(format!("output path {} escaped its root", target.display()))
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(TorrentError::metainfo("unsafe output directory"));
        };
        current.push(component);
        match symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(TorrentError::metainfo(format!(
                    "output directory {} is a symlink",
                    current.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(TorrentError::metainfo(format!(
                    "output component {} is not a directory",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_dir_all(&current).await?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

async fn reject_symlink(path: &Path) -> Result<()> {
    if symlink_metadata(path)
        .await
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(TorrentError::metainfo(format!(
            "output root {} is a symlink",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn scratch() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "pandora-torrent-storage-{}-{nonce}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn writes_piece_bytes_across_file_boundaries() {
        let info = b"d5:filesld6:lengthi3e4:pathl5:a.mkveed6:lengthi3e4:pathl5:b.mkveee4:name4:root12:piece lengthi4e6:pieces40:0000000000000000000011111111111111111111e";
        let meta = Metainfo::from_info_bytes(info, vec![], None).unwrap();
        let root = scratch();
        let mut storage = Storage::create(&root, &meta, None).await.unwrap();
        storage.write_piece(0, b"abcd").await.unwrap();
        storage.write_piece(4, b"ef").await.unwrap();
        storage.finish().await.unwrap();
        assert_eq!(
            tokio::fs::read(root.join("root/a.mkv")).await.unwrap(),
            b"abc"
        );
        assert_eq!(
            tokio::fs::read(root.join("root/b.mkv")).await.unwrap(),
            b"def"
        );
        tokio::fs::remove_dir_all(root).await.ok();
    }
}
