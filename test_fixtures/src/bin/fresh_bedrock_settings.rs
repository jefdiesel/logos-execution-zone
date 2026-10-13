//! Writes a copy of `bedrock/deployment-settings.yaml` whose chain starts now
//! and prints its path, for running the bedrock compose against a fresh chain.
//!
//! ```sh
//! BEDROCK_DEPLOYMENT_SETTINGS=$(cargo run -p test_fixtures --bin fresh_bedrock_settings) \
//!   docker compose -f bedrock/docker-compose.yml up
//! ```

#![expect(clippy::print_stdout, reason = "It's normal in this small cli")]

use std::path::PathBuf;

use anyhow::Result;
use test_fixtures::setup::fresh_deployment_settings;

fn main() -> Result<()> {
    let template =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bedrock/deployment-settings.yaml");
    println!("{}", fresh_deployment_settings(&template)?.display());
    Ok(())
}
