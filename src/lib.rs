//! Shared configuration, database, and printer facilities for the package binaries.

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

/// Prints the shared, sectioned Unix-style help used by package applications.
pub fn print_long_help(
    name: &str,
    version: &str,
    description: &str,
    usage: &str,
    options: &[(&str, &str)],
    examples: &[&str],
    config: &str,
) {
    println!("{name} {version}\n\n{description}\n\nUSAGE\n    {usage}\n\nOPTIONS");
    for (option, explanation) in options {
        println!("    {option:<24} {explanation}");
    }
    println!("\nEXAMPLES");
    for example in examples {
        println!("    {example}");
    }
    println!("\nCONFIGURATION\n{config}");
}
