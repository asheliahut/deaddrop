//! The BitTorrent side: download a torrent in leech-only mode, then remove it
//! from the session so nothing is seeded.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, Session, SessionOptions,
    api::TorrentIdOrHash,
};

/// One file from a finished torrent.
pub struct DownloadedFile {
    /// Path relative to the download dir. For a multi-file torrent this starts
    /// with the torrent's name.
    pub name: String,
    /// Absolute path on disk.
    pub path: PathBuf,
}

/// A finished torrent download.
pub struct Download {
    /// The torrent's display name (or its info hash if it has none).
    pub name: String,
    pub files: Vec<DownloadedFile>,
}

/// Largest .torrent file we accept, locally or from a reset request.
pub const MAX_TORRENT_BYTES: usize = 16 * 1024 * 1024;

/// Where a torrent comes from.
pub enum Source {
    /// A magnet URI or an http(s) URL of a .torrent file.
    Url(String),
    /// The contents of a .torrent file.
    Bytes(Vec<u8>),
}

impl Source {
    /// Parse a command-line argument: a magnet URI, an http(s) URL, or a path
    /// to a local .torrent file (which is read here).
    pub fn from_arg(arg: &str) -> Result<Self> {
        if is_url(arg) {
            return Ok(Self::Url(arg.to_string()));
        }
        let bytes =
            std::fs::read(arg).with_context(|| format!("failed to read torrent file {arg}"))?;
        Self::bytes(bytes)
    }

    /// A magnet or http(s) URL. Anything else is an error; in particular a
    /// local path is never accepted, since this is used for remote requests.
    pub fn url(url: String) -> Result<Self> {
        if !is_url(&url) {
            bail!("expected a magnet URI or an http(s) URL");
        }
        Ok(Self::Url(url))
    }

    pub fn bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() > MAX_TORRENT_BYTES {
            bail!(
                "torrent file is too large ({}, limit {})",
                human(bytes.len() as u64),
                human(MAX_TORRENT_BYTES as u64)
            );
        }
        Ok(Self::Bytes(bytes))
    }

    /// A short description for logs.
    pub fn describe(&self) -> String {
        match self {
            Self::Url(u) if u.to_ascii_lowercase().starts_with("magnet:") => "magnet link".into(),
            Self::Url(_) => "torrent URL".into(),
            Self::Bytes(b) => format!(".torrent file ({})", human(b.len() as u64)),
        }
    }

    fn into_add_torrent(self) -> AddTorrent<'static> {
        match self {
            Self::Url(u) => AddTorrent::from_url(u),
            Self::Bytes(b) => AddTorrent::from_bytes(b),
        }
    }
}

fn is_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("magnet:") || lower.starts_with("http://") || lower.starts_with("https://")
}

/// Download `source` into `dir` and return the files once they are complete.
/// When this returns, the torrent session has stopped, so nothing is seeded.
pub async fn download(source: Source, dir: &Path, peers: Vec<SocketAddr>) -> Result<Download> {
    tokio::fs::create_dir_all(dir).await?;
    let add = source.into_add_torrent();

    let session = Session::new_with_opts(
        dir.to_path_buf(),
        SessionOptions {
            // Leech only: never upload a byte to peers.
            disable_upload: true,
            // No listener, so peers can't connect to us.
            listen: None,
            // Keep DHT for magnet resolution, but don't persist state anywhere.
            dht: Some(DhtSessionConfig {
                persistence: None,
                ..Default::default()
            }),
            persistence: None,
            fastresume: false,
            ..Default::default()
        },
    )
    .await
    .context("failed to start the torrent session")?;

    let handle = match session
        .add_torrent(
            add,
            Some(AddTorrentOptions {
                overwrite: true,
                initial_peers: (!peers.is_empty()).then_some(peers),
                ..Default::default()
            }),
        )
        .await
        .context("failed to add the torrent")?
    {
        AddTorrentResponse::Added(_, h) | AddTorrentResponse::AlreadyManaged(_, h) => h,
        AddTorrentResponse::ListOnly(_) => {
            bail!("the torrent session returned a list-only response")
        }
    };

    eprintln!(
        "torrent: resolving metadata for {}",
        handle
            .name()
            .unwrap_or_else(|| handle.info_hash().as_string())
    );

    // Print progress while the download runs.
    let progress = tokio::spawn({
        let handle = handle.clone();
        async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let s = handle.stats();
                if s.total_bytes > 0 {
                    let pct = s.progress_bytes as f64 / s.total_bytes as f64 * 100.0;
                    let speed = s
                        .live
                        .as_ref()
                        .map(|l| format!(", {}", l.download_speed))
                        .unwrap_or_default();
                    eprintln!(
                        "torrent: {pct:5.1}% ({} / {}){speed}",
                        human(s.progress_bytes),
                        human(s.total_bytes)
                    );
                }
            }
        }
    });

    let res = handle.wait_until_completed().await;
    progress.abort();
    res.context("torrent download failed")?;

    let name = handle
        .name()
        .unwrap_or_else(|| handle.info_hash().as_string());
    let output_folder = handle.output_folder().to_path_buf();
    let files = handle.with_metadata(|m| {
        m.file_infos
            .iter()
            .filter(|f| !f.attrs.padding && !f.attrs.symlink)
            .map(|f| output_folder.join(&f.relative_filename))
            .collect::<Vec<_>>()
    })?;

    // Stop seeding: take the torrent out of the session (keep the files on
    // disk), then shut the whole session down (DHT, trackers, peers).
    session
        .delete(TorrentIdOrHash::Id(handle.id()), false)
        .await
        .context("failed to remove the torrent from the session")?;
    session.stop().await;
    drop(handle);
    eprintln!("torrent: download complete, seeding stopped");

    let files = files
        .into_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(dir)
                .with_context(|| format!("{} is outside the download dir", path.display()))?;
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            Ok(DownloadedFile { name, path })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Download { name, files })
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.2} {}", UNITS[i])
}
