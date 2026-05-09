use std::path::PathBuf;
use tokio::{fs::File, io::AsyncWriteExt};
use std::fs::File as StdFile;
use std::io::Write;


pub struct LoggingHandle {
    path: PathBuf,
    file: File,
    buf: String,
}

impl Drop for LoggingHandle {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }

        if let Some(parent) = self.path.parent() {
            let drop_path = parent.join(
                format!(
                    "{}.impl_drop.log",
                    self.path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )
            );

            if let Ok(mut f) = StdFile::create(drop_path) {
                let _ = f.write_all(self.buf.as_bytes());
            }
        }
    }
}
/// takes an Option<LoggingHandle> and logs $s: &str
#[macro_export]
macro_rules! log {
    ($handle: expr, $s: expr) => {
        if let Some(h) = &mut $handle {
            h.write($s).await;
        }
    };
}

impl LoggingHandle {
    pub async fn get_handle(path: &PathBuf) -> Result<Self, ()> {
        Ok(Self {
            path: path.clone(),
            file: match File::create(path).await {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{e}");
                    return Err(());
                }
            },
            buf: String::new()
        })
    }
    pub async fn flush(&mut self) {
        if !self.buf.is_empty() {
            match self.file.write_all(self.buf.as_bytes()).await {
                Ok(_) => {
                    self.buf.clear();
                },
                Err(e) => {
                    eprintln!("{e}");
                }
            };
        }
    }
    pub async fn write(&mut self, s: &str) {
        if self.buf.len() > 5000 {
            self.flush().await;
        }
        self.buf.push_str(s);
    }
    pub async fn clear(&mut self) {
        self.buf.clear();
    }
}
