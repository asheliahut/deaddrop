//! What `serve` serves: a torrent, local files and directories, stdin, or a
//! file downloaded over http(s).
//!
//! [`Source::from_args`] works out which one the command line means, and
//! [`fetch`] turns it into a list of files on disk, ready to import.

use std::{
    io::IsTerminal,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use indicatif::ProgressBar;
use n0_future::TryStreamExt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::{
    torrent::{self, MAX_TORRENT_BYTES, human},
    ui,
};

/// Where the data to serve comes from.
pub enum Source {
    /// A magnet URI, a .torrent URL, or a .torrent file's contents.
    Torrent(torrent::Source),
    /// Files and directories on this machine, served in place. Absolute.
    Paths(Vec<PathBuf>),
    /// Whatever is piped in, served as one file.
    Stdin { name: String },
    /// An http(s) URL that deaddrop downloads itself. If the response turns
    /// out to be a .torrent file (and `as_is` is off), the torrent is
    /// downloaded instead.
    Download {
        url: String,
        name: Option<String>,
        as_is: bool,
    },
}

/// One file to serve.
pub struct Entry {
    /// Its path in the collection, `/`-separated.
    pub name: String,
    /// Absolute path on disk.
    pub path: PathBuf,
}

/// The files of a source, on disk and ready to import.
pub struct Fetched {
    /// A display name for logs and notifications.
    pub name: String,
    pub files: Vec<Entry>,
    /// Whether deaddrop made these files (a torrent, stdin or a download)
    /// and may delete them. False for the user's own files, which are never
    /// deleted or changed.
    pub owned: bool,
}

/// Default file name for stdin without `--name`.
const STDIN_NAME: &str = "stdin";
/// Default file name for a download whose URL and headers give none.
const DOWNLOAD_NAME: &str = "download";

impl Source {
    /// Work out the source from `serve`'s arguments.
    ///
    /// - `magnet:...` is a torrent.
    /// - `-` is stdin.
    /// - An http(s) URL whose path ends in `.torrent` is a torrent, as in
    ///   2.0; any other http(s) URL is downloaded (and still treated as a
    ///   torrent if the response is one, unless `as_is`).
    /// - A local file ending in `.torrent` is a torrent (unless `as_is`);
    ///   any other file or directory is served as it is.
    ///
    /// Several arguments must all be local files or directories.
    pub fn from_args(args: &[String], name: Option<String>, as_is: bool) -> Result<Self> {
        let name = name
            .filter(|n| !n.is_empty())
            .map(sanitize_name)
            .transpose()?;
        let source = match args {
            [] => bail!("nothing to serve"),
            [one] => Self::from_arg(one, name.clone(), as_is)?,
            many => {
                let mut paths = Vec::with_capacity(many.len());
                for arg in many {
                    ensure!(
                        !is_special(arg),
                        "{arg}: only local files and directories can be served together; \
                         a torrent, URL or stdin must be the only source"
                    );
                    match Self::from_arg(arg, None, as_is)? {
                        Self::Paths(p) => paths.extend(p),
                        _ => bail!(
                            "{arg} is a .torrent file; only one torrent can be served at a time \
                             (use --as-is to serve .torrent files as they are)"
                        ),
                    }
                }
                Self::Paths(paths)
            }
        };
        match (&source, name.is_some(), as_is) {
            (Self::Paths(_), true, _) | (Self::Torrent(_), true, _) => {
                bail!("--name only applies to stdin (`-`) or an http(s) download")
            }
            (Self::Torrent(torrent::Source::Url(u)), _, true) if is_magnet(u) => {
                bail!("--as-is can't serve a magnet link; it has no file to serve")
            }
            _ => {}
        }
        Ok(source)
    }

    fn from_arg(arg: &str, name: Option<String>, as_is: bool) -> Result<Self> {
        if arg == "-" {
            return Ok(Self::Stdin {
                name: name.unwrap_or_else(|| STDIN_NAME.into()),
            });
        }
        if is_magnet(arg) {
            return Ok(Self::Torrent(torrent::Source::Url(arg.into())));
        }
        if is_http(arg) {
            if !as_is && url_path_is_torrent(arg) {
                // librqbit fetches it, as in 2.0.
                return Ok(Self::Torrent(torrent::Source::Url(arg.into())));
            }
            return Ok(Self::Download {
                url: arg.into(),
                name,
                as_is,
            });
        }
        let path = Path::new(arg);
        let meta = std::fs::metadata(path).with_context(|| format!("can't read {arg}"))?;
        if meta.is_file() && !as_is && has_torrent_ext(arg) {
            let bytes = std::fs::read(path)
                .with_context(|| format!("failed to read torrent file {arg}"))?;
            return Ok(Self::Torrent(torrent::Source::bytes(bytes)?));
        }
        ensure!(
            meta.is_file() || meta.is_dir(),
            "{arg} is not a regular file or a directory (to read a pipe, use `-` for stdin)"
        );
        // The blob store needs absolute paths, and this also resolves `.`
        // and symlinks given on the command line.
        let abs = std::fs::canonicalize(path).with_context(|| format!("can't resolve {arg}"))?;
        Ok(Self::Paths(vec![abs]))
    }
}

/// Get the files of `source`. Downloads (torrents, stdin, http) go into
/// `dir`; local paths stay where they are. Files under `exclude` (the
/// server's own data dir) are never served.
pub async fn fetch(
    source: Source,
    dir: &Path,
    peers: &[SocketAddr],
    exclude: &Path,
) -> Result<Fetched> {
    match source {
        Source::Torrent(t) => from_torrent(t, dir, peers).await,
        Source::Paths(paths) => {
            let exclude = exclude.to_path_buf();
            tokio::task::spawn_blocking(move || walk(&paths, &exclude)).await?
        }
        Source::Stdin { name } => {
            tokio::fs::create_dir_all(dir).await?;
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                say!("reading from the terminal; end with Ctrl-D");
            }
            let path = dir.join(&name);
            say!("stdin: reading into {name}");
            let n = copy_with_progress(tokio::io::stdin(), &path, "stdin", None).await?;
            say!("stdin: read {}", human(n));
            Ok(single_file(name, path))
        }
        Source::Download { url, name, as_is } => match download(&url, name, as_is, dir).await? {
            Downloaded::File(name, path) => Ok(single_file(name, path)),
            Downloaded::Torrent(bytes) => {
                say!("download: the response is a .torrent file; downloading the torrent");
                from_torrent(torrent::Source::bytes(bytes)?, dir, peers).await
            }
        },
    }
}

async fn from_torrent(t: torrent::Source, dir: &Path, peers: &[SocketAddr]) -> Result<Fetched> {
    let torrent::Download { name, files } = torrent::download(t, dir, peers.to_vec()).await?;
    Ok(Fetched {
        name,
        files,
        owned: true,
    })
}

fn single_file(name: String, path: PathBuf) -> Fetched {
    Fetched {
        name: name.clone(),
        files: vec![Entry { name, path }],
        owned: true,
    }
}

// ---------------------------------------------------------------------------
// Local files
// ---------------------------------------------------------------------------

/// List the regular files under `paths`. A directory's files are named
/// `<dir name>/<path inside it>`, like a multi-file torrent. Symlinks inside
/// directories and special files are skipped; empty directories aren't
/// served (a collection only holds files).
fn walk(paths: &[PathBuf], exclude: &Path) -> Result<Fetched> {
    let mut files = Vec::new();
    for path in paths {
        ensure!(
            !path.starts_with(exclude),
            "{} is inside the server's data dir ({}), which deaddrop cleans up; \
             serve it from somewhere else",
            path.display(),
            exclude.display()
        );
        let base = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            walk_dir(path, &base, exclude, &mut files)?;
        } else {
            files.push(Entry {
                name: base,
                path: path.clone(),
            });
        }
    }
    let mut seen = std::collections::HashSet::new();
    for f in &files {
        ensure!(
            seen.insert(f.name.as_str()),
            "two of the paths to serve would both be named {:?}; rename one, or serve their \
             parent directory instead",
            f.name
        );
    }
    ensure!(
        !files.is_empty(),
        "no files to serve in {}",
        paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let name = match paths {
        [one] => one
            .file_name()
            .map_or_else(|| one.display().to_string(), |n| n.to_string_lossy().into()),
        many => format!("{} items", many.len()),
    };
    Ok(Fetched {
        name,
        files,
        owned: false,
    })
}

fn walk_dir(dir: &Path, prefix: &str, exclude: &Path, out: &mut Vec<Entry>) -> Result<()> {
    if dir.starts_with(exclude) {
        // The data dir (e.g. the temp dir) sits inside what's being served.
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("can't read directory {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("can't read directory {}", dir.display()))?;
    // Same order every time, so the same files give the same ticket hash.
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let file_name = e.file_name().to_string_lossy().into_owned();
        let name = if prefix.is_empty() {
            file_name
        } else {
            format!("{prefix}/{file_name}")
        };
        let ty = e
            .file_type()
            .with_context(|| format!("can't read {}", path.display()))?;
        if ty.is_dir() {
            walk_dir(&path, &name, exclude, out)?;
        } else if ty.is_file() {
            out.push(Entry { name, path });
        } else if ty.is_symlink() {
            say!("skipping symlink {}", path.display());
        } else {
            say!("skipping {} (not a regular file)", path.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// http(s) downloads
// ---------------------------------------------------------------------------

enum Downloaded {
    /// A file to serve: its name and where it is.
    File(String, PathBuf),
    /// The response was a .torrent file.
    Torrent(Vec<u8>),
}

async fn download(url: &str, name: Option<String>, as_is: bool, dir: &Path) -> Result<Downloaded> {
    tokio::fs::create_dir_all(dir).await?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("deaddrop/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .build()?;
    say!("download: requesting {}", redact(url));
    let resp = client
        .get(url)
        .send()
        .await
        .context("download failed")?
        .error_for_status()
        .context("download failed")?;

    let header = |h: reqwest::header::HeaderName| {
        resp.headers()
            .get(h)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    if !as_is
        && header(reqwest::header::CONTENT_TYPE).is_some_and(|ct| {
            ct.to_ascii_lowercase()
                .starts_with("application/x-bittorrent")
        })
    {
        return Ok(Downloaded::Torrent(
            read_capped(resp, MAX_TORRENT_BYTES).await?,
        ));
    }

    let name = match name {
        Some(n) => n,
        None => header(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|cd| disposition_filename(&cd))
            .or_else(|| url_filename(resp.url()))
            .and_then(|n| sanitize_name(n).ok())
            .unwrap_or_else(|| DOWNLOAD_NAME.into()),
    };
    let path = dir.join(&name);
    say!("download: saving as {name}");
    let len = resp.content_length();
    let body =
        tokio_util::io::StreamReader::new(resp.bytes_stream().map_err(std::io::Error::other));
    copy_with_progress(body, &path, "download", len).await?;

    if !as_is && looks_like_torrent(&path).await {
        let bytes = tokio::fs::read(&path).await?;
        tokio::fs::remove_file(&path).await.ok();
        return Ok(Downloaded::Torrent(bytes));
    }
    Ok(Downloaded::File(name, path))
}

/// Read a whole response body, refusing one larger than `cap`.
async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.context("download failed")? {
        ensure!(
            buf.len() + chunk.len() <= cap,
            "the .torrent file is too large (limit {})",
            human(cap as u64)
        );
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Whether a downloaded file is small enough to be a .torrent file and
/// starts like one: a bencoded dictionary whose keys include `info`.
async fn looks_like_torrent(path: &Path) -> bool {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return false;
    };
    if meta.len() > MAX_TORRENT_BYTES as u64 {
        return false;
    }
    let Ok(bytes) = tokio::fs::read(path).await else {
        return false;
    };
    is_torrent_bytes(&bytes)
}

fn is_torrent_bytes(b: &[u8]) -> bool {
    // `d`, then a key: `<len>:<key>`.
    let Some(rest) = b.strip_prefix(b"d") else {
        return false;
    };
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || rest.get(digits) != Some(&b':') {
        return false;
    }
    b.windows(7).any(|w| w == b"4:infod")
}

/// `filename*=UTF-8''...` or `filename="..."` from a Content-Disposition.
fn disposition_filename(cd: &str) -> Option<String> {
    let mut plain = None;
    for part in cd.split(';').map(str::trim) {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.trim().to_ascii_lowercase().as_str() {
            "filename*" => {
                // charset'lang'percent-encoded
                let enc = v.trim().splitn(3, '\'').nth(2)?;
                return Some(percent_decode(enc));
            }
            "filename" => plain = Some(v.trim().trim_matches('"').to_string()),
            _ => {}
        }
    }
    plain
}

/// The last non-empty path segment of a URL, percent-decoded.
fn url_filename(url: &reqwest::Url) -> Option<String> {
    url.path_segments()?
        .rev()
        .find(|s| !s.is_empty())
        .map(percent_decode)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Make a name safe to use as one file name: no directories, no `..`, no
/// control characters.
fn sanitize_name(name: String) -> Result<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    ensure!(
        !clean.is_empty() && clean != "." && clean != "..",
        "{name:?} is not a usable file name"
    );
    Ok(clean.to_string())
}

/// A URL without its query string and user info, for logs: those often
/// hold tokens.
fn redact(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut u) => {
            let had_query = u.query().is_some();
            u.set_query(None);
            u.set_username("").ok();
            u.set_password(None).ok();
            let mut s = u.to_string();
            if had_query {
                s.push_str("?…");
            }
            s
        }
        Err(_) => "(invalid URL)".into(),
    }
}

// ---------------------------------------------------------------------------
// Copying with progress
// ---------------------------------------------------------------------------

/// Copy `from` into a new file at `to`, showing progress. Returns the bytes
/// copied.
async fn copy_with_progress(
    mut from: impl AsyncRead + Unpin,
    to: &Path,
    label: &str,
    len: Option<u64>,
) -> Result<u64> {
    let file = tokio::fs::File::create(to)
        .await
        .with_context(|| format!("can't create {}", to.display()))?;
    let mut file = tokio::io::BufWriter::with_capacity(1 << 20, file);
    let mut meter = Meter::new(label, len);
    let mut buf = vec![0u8; 256 * 1024];
    let res: Result<u64> = async {
        let mut total = 0u64;
        loop {
            let n = from
                .read(&mut buf)
                .await
                .with_context(|| format!("{label} failed"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])
                .await
                .with_context(|| format!("can't write {}", to.display()))?;
            total += n as u64;
            meter.set(total);
        }
        file.flush().await?;
        Ok(total)
    }
    .await;
    meter.finish(res.as_ref().ok().copied());
    if let (Some(expected), Ok(got)) = (len, &res) {
        ensure!(
            *got == expected,
            "{label} ended early: got {} of {}",
            human(*got),
            human(expected)
        );
    }
    res
}

/// Progress for a copy: a bar on a terminal, otherwise a line every few
/// seconds.
struct Meter {
    label: String,
    len: Option<u64>,
    bar: Option<ProgressBar>,
    every: ui::Every,
    started: Instant,
    pos: u64,
}

impl Meter {
    fn new(label: &str, len: Option<u64>) -> Self {
        Self {
            label: label.into(),
            len,
            bar: ui::transfer_bar(label, len),
            every: ui::Every::new(Duration::from_secs(2)),
            started: Instant::now(),
            pos: 0,
        }
    }

    fn set(&mut self, pos: u64) {
        self.pos = pos;
        if let Some(bar) = &self.bar {
            bar.set_position(pos);
        } else if self.every.due() {
            match self.len {
                Some(len) if len > 0 => say!(
                    "{}: {:5.1}% ({} / {})",
                    self.label,
                    pos as f64 / len as f64 * 100.0,
                    human(pos),
                    human(len)
                ),
                _ => say!("{}: {}", self.label, human(pos)),
            }
        }
    }

    fn finish(self, done: Option<u64>) {
        let Some(bar) = self.bar else { return };
        match done {
            Some(n) => ui::finish_bar(
                &bar,
                format!(
                    "{}: {}",
                    self.label,
                    ui::rate_summary(n, self.started.elapsed())
                ),
            ),
            None => ui::fail_bar(
                &bar,
                format!("{}: failed after {}", self.label, human(self.pos)),
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn is_magnet(s: &str) -> bool {
    s.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("magnet:"))
}

fn is_http(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Arguments that can't be combined with others.
fn is_special(s: &str) -> bool {
    s == "-" || is_magnet(s) || is_http(s)
}

fn has_torrent_ext(s: &str) -> bool {
    s.to_ascii_lowercase().ends_with(".torrent")
}

fn url_path_is_torrent(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| has_torrent_ext(u.path()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn classifies_arguments() {
        let s = |a: &[&str], as_is| Source::from_args(&args(a), None, as_is).unwrap();
        assert!(matches!(
            s(&["magnet:?xt=urn:btih:x"], false),
            Source::Torrent(_)
        ));
        assert!(matches!(s(&["-"], false), Source::Stdin { name } if name == "stdin"));
        assert!(matches!(
            s(&["https://example.com/a/x.torrent?k=1"], false),
            Source::Torrent(torrent::Source::Url(_))
        ));
        assert!(matches!(
            s(&["https://example.com/a/x.torrent"], true),
            Source::Download { as_is: true, .. }
        ));
        assert!(matches!(
            s(&["https://example.com/a/x.iso"], false),
            Source::Download { as_is: false, .. }
        ));
        assert!(
            matches!(s(&["src"], false), Source::Paths(p) if p.len() == 1 && p[0].is_absolute())
        );
        assert!(matches!(s(&["src", "Cargo.toml"], false), Source::Paths(p) if p.len() == 2));
    }

    #[test]
    fn rejects_bad_combinations() {
        let e = |a: &[&str], name: Option<&str>, as_is| {
            Source::from_args(&args(a), name.map(Into::into), as_is).is_err()
        };
        assert!(e(&["src", "-"], None, false));
        assert!(e(&["src", "magnet:?xt=urn:btih:x"], None, false));
        assert!(e(&["src"], Some("x"), false));
        assert!(e(&["magnet:?xt=urn:btih:x"], None, true));
        assert!(e(&["-"], Some(".."), false));
        assert!(e(&["/definitely/not/here"], None, false));
        assert!(e(&[], None, false));
    }

    #[test]
    fn walks_directories_without_escaping_the_data_dir() {
        let root = std::env::temp_dir().join(format!("deaddrop-walk-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("d/sub")).unwrap();
        std::fs::create_dir_all(root.join("d/data")).unwrap();
        std::fs::write(root.join("d/b.txt"), "b").unwrap();
        std::fs::write(root.join("d/sub/a.txt"), "a").unwrap();
        std::fs::write(root.join("d/data/skip"), "x").unwrap();
        let f = walk(&[root.join("d")], &root.join("d/data")).unwrap();
        let names: Vec<_> = f.files.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["d/b.txt", "d/sub/a.txt"]);
        assert!(!f.owned);
        assert_eq!(f.name, "d");
        // The served path itself inside the data dir is refused.
        assert!(walk(&[root.join("d/data")], &root.join("d")).is_err());
        // Two files with the same name.
        std::fs::write(root.join("b.txt"), "b").unwrap();
        std::fs::create_dir_all(root.join("e")).unwrap();
        std::fs::write(root.join("e/b.txt"), "b").unwrap();
        assert!(
            walk(
                &[root.join("b.txt"), root.join("e/b.txt")],
                Path::new("/nonexistent")
            )
            .is_err()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn names_from_headers_and_urls() {
        assert_eq!(
            disposition_filename(r#"attachment; filename="a b.iso""#).as_deref(),
            Some("a b.iso")
        );
        assert_eq!(
            disposition_filename("attachment; filename=x.bin; filename*=UTF-8''%C3%A9t%C3%A9.txt")
                .as_deref(),
            Some("été.txt")
        );
        assert_eq!(disposition_filename("inline"), None);
        let u = reqwest::Url::parse("https://e.com/dir/my%20file.tar.gz?x=1").unwrap();
        assert_eq!(url_filename(&u).as_deref(), Some("my file.tar.gz"));
        let u = reqwest::Url::parse("https://e.com/").unwrap();
        assert_eq!(url_filename(&u), None);
        assert_eq!(sanitize_name("../../etc/passwd".into()).unwrap(), "passwd");
        assert_eq!(sanitize_name(r"C:\x\y.txt".into()).unwrap(), "y.txt");
        assert!(sanitize_name("..".into()).is_err());
        assert!(sanitize_name("dir/".into()).is_err());
    }

    #[test]
    fn sniffs_torrents() {
        assert!(is_torrent_bytes(b"d8:announce3:url4:infod4:name1:xee"));
        assert!(is_torrent_bytes(b"d4:infod6:lengthi1eee"));
        assert!(!is_torrent_bytes(b"PK\x03\x04 4:infod"));
        assert!(!is_torrent_bytes(b"d:4:infod"));
        assert!(!is_torrent_bytes(b"hello"));
    }

    #[test]
    fn redacts_urls() {
        assert_eq!(
            redact("https://u:p@e.com/x.iso?token=secret"),
            "https://e.com/x.iso?…"
        );
        assert_eq!(redact("https://e.com/x.iso"), "https://e.com/x.iso");
    }
}
