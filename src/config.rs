use serde::Deserialize;
use std::{env, error::Error, fs, io, path::Path};

#[derive(Debug, Deserialize)]
/// Shared connection settings loaded by every binary in this Cargo package.
pub struct AppConfig {
    /// MySQL URL, including credentials, host, and optional default database.
    pub mysql_url: String,
}

impl AppConfig {
    /// Finds `config.toml` above the executable or working directory and loads it.
    pub fn load() -> Result<Self, Box<dyn Error>> {
        let mut search_directories = Vec::new();

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

        Self::from_path(config_path)
    }

    /// Loads and deserializes a TOML config file at an explicit path.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, Box<dyn Error>> {
        let contents = fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }
}
