//! Test helper: seed a local file or directory on 127.0.0.1 so you can try
//! `deaddrop serve` without the public swarm.
//!
//!   cargo run --example seed -- <path>
//!
//! It writes `<path>.torrent`, then prints the listen address and a magnet link.

use std::path::PathBuf;

use anyhow::Context;
use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerOptions, Session, SessionOptions,
    create_torrent, spawn_utils::BlockingSpawner,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .context("usage: seed <path>")?
        .into();
    let path = path.canonicalize()?;
    let created = create_torrent(
        &path,
        CreateTorrentOptions {
            name: None,
            trackers: vec![],
            piece_length: None,
        },
        &BlockingSpawner::new(4),
    )
    .await?;
    let bytes = created.as_bytes()?;
    let torrent_path = path.with_extension("torrent");
    std::fs::write(&torrent_path, &bytes)?;

    let session = Session::new_with_opts(
        created.output_folder.clone(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            listen: Some(ListenerOptions {
                listen_addr: "127.0.0.1:0".parse()?,
                ipv4_only: true,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await?;
    session
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                output_folder: Some(created.output_folder.to_string_lossy().into_owned()),
                overwrite: true,
                ..Default::default()
            }),
        )
        .await?;

    println!("torrent: {}", torrent_path.display());
    println!("magnet:  {}", created.as_magnet());
    println!(
        "peer:    {}",
        session.listen_addr().context("not listening")?
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}
