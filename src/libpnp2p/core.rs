use qbit_rs::Qbit;
use qbit_rs::model::{
    AddTorrentArg, Credential, GetTorrentListArg, TorrentFile, TorrentSource
};
use std::io::{self, Write};
use tokio::time::{sleep, Duration};
use tokio::fs;

pub struct P2p {
    api: Qbit,
}

impl P2p {
    pub async fn new(uname: &str, pass: &str) -> Self {
        let credential = Credential::new(uname, pass);

        Self {
            api: Qbit::new("http://localhost:8089", credential),
        }
    }

    pub async fn download_and_remove(
        &self,
        torrent_path: &str,
        save_path: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {

        // 1️⃣ Read .torrent file into memory
        let torrent_bytes = fs::read(torrent_path).await?;

        let add_args = AddTorrentArg::builder()
            .source(TorrentSource::TorrentFiles {
                torrents: vec![TorrentFile {
                    filename: torrent_path.to_string(),
                    data: torrent_bytes,
                }],
            })
            .savepath(save_path.to_string())
            .build();

        self.api.add_torrent(add_args).await?;

        // 2️⃣ Find the torrent hash
        sleep(Duration::from_secs(1)).await;

        let torrents = self.api
            .get_torrent_list(GetTorrentListArg::builder().build())
            .await?;

        let torrent = torrents
            .iter()
            .max_by_key(|t| t.added_on.unwrap_or(0))
            .ok_or("No torrents found")?;

        let hash = torrent.hash.clone().unwrap();

        // 3️⃣ Progress loop
        let mut last_printed = -1.0;

        loop {
            let props = self.api.get_torrent_properties(&hash).await?;
            let percent = props.total_downloaded.unwrap() as f32
                / props.total_size.unwrap() as f32 * 100.0;


            if (percent - last_printed).abs() >= 0.1 {
                print!("%{:.1}\n", percent);
                io::stdout().flush().ok();
                last_printed = percent;
            }

            if percent >= 100 as f32 {
                break;
            }

            sleep(Duration::from_secs(1)).await;
        }

        println!("%DONE");

        self.api.delete_torrents(vec![hash], false).await?;

        Ok(())
    }
}

