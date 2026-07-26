#[path = "src/config_model.rs"]
mod config_model;

use std::{env, fs, path::PathBuf};

use schemars::schema_for;

fn main() {
    println!("cargo:rerun-if-changed=src/config_model.rs");

    let schema = schema_for!(config_model::Config);
    let contents = serde_json::to_string_pretty(&schema).expect("serialize config schema");
    let schema_directory =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest directory")).join("schemas");
    fs::create_dir_all(&schema_directory).expect("create schema directory");
    let path = schema_directory.join("konnect.config.schema.json");
    let current = fs::read_to_string(&path).ok();
    if current.as_deref() != Some(&contents) {
        fs::write(path, format!("{contents}\n")).expect("write config schema");
    }
}
