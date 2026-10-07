use serde::Deserialize;
use std::{
    env,
    error::Error,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
/// Shared connection settings loaded by every binary in this Cargo package.
pub struct AppConfig {
    /// MySQL URL, including credentials, host, and optional default database.
    pub mysql_url: String,
    /// Optional production line used when the CLI does not specify `-l`.
    #[serde(default)]
    pub default_line: Option<u32>,
    /// IP address of the raw TCP printer used by `rusend`.
    #[serde(default)]
    pub printer_ip: Option<String>,
    /// TCP port of the raw printer service.
    #[serde(default)]
    pub printer_port: Option<u16>,
    /// Numeric printer identifier stored in autosave records.
    #[serde(default)]
    pub printer_id: Option<u32>,
}

impl AppConfig {
    /// Finds `config.toml` above the executable or working directory and loads it.
    /// This also lets binaries launched from `target/release` find the project config.
    pub fn load() -> Result<Self, Box<dyn Error>> {
        Self::load_with_path().map(|(config, _)| config)
    }

    /// Loads the config and returns the path used to find it.
    pub fn load_with_path() -> Result<(Self, PathBuf), Box<dyn Error>> {
        let mut search_directories = Vec::new();

        // Prefer the executable's ancestors, then fall back to the launch directory.
        if let Ok(executable) = env::current_exe() {
            if let Some(directory) = executable.parent() {
                search_directories.extend(directory.ancestors().map(Path::to_path_buf));
            }
        }

        if let Ok(directory) = env::current_dir() {
            search_directories.extend(directory.ancestors().map(Path::to_path_buf));
        }

        let config_path = search_directories
            .iter()
            .map(|directory| directory.join("config.toml"))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "config.toml not found above executable or working directory",
                )
            })?;

        let config = Self::from_path(&config_path)?;
        Ok((config, config_path))
    }

    /// Loads and deserializes a TOML config file at an explicit path.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, Box<dyn Error>> {
        let contents = fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }
}
