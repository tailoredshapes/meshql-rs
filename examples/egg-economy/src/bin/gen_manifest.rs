//! Regenerate config/manifest.json. Run from anywhere:
//!   cargo run -p egg-economy --bin gen_manifest

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
    let manifest = egg_economy::manifest::generate(&config_dir)?;
    let out = config_dir.join("manifest.json");
    std::fs::write(&out, serde_json::to_string_pretty(&manifest)? + "\n")?;
    println!("wrote {}", out.display());
    Ok(())
}
