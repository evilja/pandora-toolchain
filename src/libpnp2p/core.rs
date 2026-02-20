use std::path::PathBuf;
use std::str::FromStr;

use qbit_rs::Qbit;
use qbit_rs::model::{
    AddTorrentArg, Credential, GetTorrentListArg, Sep, TorrentFile, TorrentSource
};
use tokio::time::{sleep, Duration};
use tokio::fs::{self, try_exists};
use crate::{lib_pn_data, lib_pn_emit, lib_pn_schema};
use crate::libpnprotocol::core::{Protocol, Schema};


pub struct P2p {
    api: Qbit,
    cfile: Option<PathBuf>,
}

impl P2p {
    pub async fn new(uname: &str, pass: &str, cfile: Option<String>) -> Self {
        let credential = Credential::new(uname, pass);
        Self {
            api: Qbit::new("http://localhost:8089", credential),
            cfile: match cfile {
                Some(str) => {
                    Some(PathBuf::from(str))
                }
                None => { None }
            },
        }
    }

    pub async fn download_and_remove(
        &self,
        torrent_path: &str,
        save_path: &str,
        proto: Protocol,
        neg: String,
        srcmgn: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source: TorrentSource;
        if srcmgn {
            source = TorrentSource::Urls { urls: Sep::<reqwest::Url, '\n'>::from_str(torrent_path)? }
        } else {
            let torrent_bytes = fs::read(torrent_path).await?;
            source = TorrentSource::TorrentFiles {
                torrents: vec![TorrentFile {
                    filename: torrent_path.to_string(),
                    data: torrent_bytes,
                }],
            };
        }

        let add_args = AddTorrentArg::builder()
            .source(source)
            .savepath(save_path.to_string())
            .build();

        self.api.add_torrent(add_args).await?;

        sleep(Duration::from_secs(1)).await;

        let torrents = self.api
            .get_torrent_list(GetTorrentListArg::builder().build())
            .await?;

        let torrent = torrents
            .iter()
            .max_by_key(|t| t.added_on.unwrap_or(0))
            .ok_or("No torrents found")?;

        let hash = torrent.hash.clone().unwrap();


        let mut last_printed = -1.0;

        let mut percent: f32;
        let mut downloaded: f32;
        let mut total: f32;
        loop {

            let props = self.api.get_torrent_properties(&hash).await?;
            downloaded = (props.total_downloaded.unwrap() as f32).abs();
            total = (props.total_size.unwrap() as f32).abs();
            percent = (downloaded / total * 100.0).abs();

            if let Some(ref cancelfile) = self.cfile {
                if try_exists(cancelfile).await.unwrap_or(false) {
                    self.api.delete_torrents(vec![hash.clone()], true).await?;
                    println!("{}",
                        lib_pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf],
                            data   = ["3", "CANCELFILE"]
                        ).unwrap()
                    );
                    return Ok(());
                }
            }
            if percent >= 100 as f32 {
                break;
            }

            if (percent - last_printed).abs() >= 0.1 {
                let hpercent = percent.floor();
                println!("{}",
                    lib_pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, [leaf, leaf, leaf]],
                        data   = ["0", [hpercent, downloaded, total]]
                    ).unwrap()
                );
                last_printed = percent;
            }


            sleep(Duration::from_secs(5)).await;
        }

        println!("{}",
            lib_pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data   = ["1", "DONE"]
            ).unwrap()
        );

        self.api.delete_torrents(vec![hash], false).await?;

        Ok(())
    }
}
