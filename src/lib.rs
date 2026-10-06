pub mod config;
pub mod db;
pub mod printer;

/// Prints the standard four-line overview shared by command-line binaries.
pub fn print_minimal_help(
    name: &str,
    version: &str,
    description: &str,
    syntax: &str,
    config_example: &str,
) {
    println!("{name} {version}");
    println!("{description}");
    println!("Usage: {syntax}");
    println!("Config: {config_example}");
}
