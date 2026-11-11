//! TOML config file.
//!
//! Precedence: command-line flag > environment variable > config file > default.
//!
//! The file is loaded from `--config <path>` (or `DEADDROP_CONFIG`). If
//! neither is set, the platform config dir is checked
//! (`~/.config/deaddrop/config.toml` on Linux). A missing default file is
//! fine; a missing file you asked for is an error.
//!
//! Relative paths in the file are resolved against the file's directory.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::notify::NotifierConfig;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Log filter, same syntax as RUST_LOG (e.g. "info", "deaddrop=debug").
    pub log: Option<String>,
    pub serve: ServeConfig,
    pub fetch: FetchConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeConfig {
    /// Keep serving after the first download. Defaults to false (single use).
    pub multi_use: Option<bool>,
    pub data_dir: Option<PathBuf>,
    pub peers: Vec<SocketAddr>,
    pub notify: Vec<NotifierConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FetchConfig {
    pub out: Option<PathBuf>,
}

/// The default config file location for this platform, if one can be found.
pub fn default_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "deaddrop").map(|d| d.config_dir().join("config.toml"))
}

/// Where the config file comes from.
pub enum Location {
    /// Given with `--config` or `DEADDROP_CONFIG`; must exist.
    Explicit(PathBuf),
    /// The platform default; fine if it doesn't exist.
    Default(PathBuf),
    /// No platform config dir could be found.
    None,
}

pub fn locate(explicit: Option<&Path>) -> Location {
    match (explicit, default_path()) {
        (Some(p), _) => Location::Explicit(p.to_path_buf()),
        (None, Some(p)) => Location::Default(p),
        (None, None) => Location::None,
    }
}

/// Load the config. `explicit` comes from `--config` or `DEADDROP_CONFIG`.
/// Returns the config and the file it came from, if any.
pub fn load(explicit: Option<&Path>) -> Result<(Config, Option<PathBuf>)> {
    let path = match locate(explicit) {
        Location::Explicit(p) => p,
        Location::Default(p) if p.is_file() => p,
        _ => return Ok((Config::default(), None)),
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let mut config =
        parse(&text).with_context(|| format!("invalid config file {}", path.display()))?;

    let base = path.parent().unwrap_or(Path::new("."));
    if let Some(d) = &mut config.serve.data_dir {
        *d = base.join(&*d);
    }
    if let Some(o) = &mut config.fetch.out {
        *o = base.join(&*o);
    }

    warn_if_world_readable(&path, &config);
    Ok((config, Some(path)))
}

/// The commented example config, also used by `deaddrop config example`.
pub const EXAMPLE: &str = include_str!("../config.example.toml");

fn parse(text: &str) -> Result<Config> {
    // toml's error already points at the line and column.
    toml::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Webhook URLs are secrets; tell the user if other users can read them.
#[cfg(unix)]
fn warn_if_world_readable(path: &Path, config: &Config) {
    use std::os::unix::fs::PermissionsExt;
    if config.serve.notify.is_empty() {
        return;
    }
    if let Ok(meta) = std::fs::metadata(path)
        && meta.permissions().mode() & 0o077 != 0
    {
        eprintln!(
            "warning: {} contains webhook URLs but can be read by other users; consider `chmod 600 {}`",
            path.display(),
            path.display()
        );
    }
}

#[cfg(not(unix))]
fn warn_if_world_readable(_: &Path, _: &Config) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_example_parses() {
        let c = parse(EXAMPLE).unwrap();
        assert_eq!(c.serve.multi_use, Some(false));
        assert_eq!(c.serve.notify.len(), 2);
    }

    #[test]
    fn empty_is_default() {
        let c = parse("").unwrap();
        assert!(c.serve.notify.is_empty() && c.fetch.out.is_none());
    }

    #[test]
    fn rejects_unknown_keys_and_types() {
        assert!(parse("[serve]\nmulti_uze = true").is_err());
        assert!(parse("[[serve.notify]]\ntype = \"pigeon\"\nurl = \"https://x\"").is_err());
        assert!(parse("[[serve.notify]]\ntype = \"discord\"").is_err());
        assert!(parse("[serve]\npeers = [\"not-an-addr\"]").is_err());
    }
}
