//! `pim mesh` subcommand — read-only insight into the local node's
//! mesh-membership configuration.
//!
//! Today there is one subcommand: `pim mesh status`. It prints:
//!
//! - The configured mode (`open` / `private`).
//! - The mesh label (`mesh_id`) when set.
//! - The 8-byte derived fingerprint when in private mode — the same
//!   value the daemon emits on startup and exposes via the JSON-RPC
//!   `mesh.status` method, so an operator can confirm two nodes share
//!   a mesh without comparing passphrases.
//!
//! The passphrase itself is never printed.
//!
//! Implementation note: this command derives the fingerprint locally
//! using the same `pim_crypto::MeshSecret` KDF the daemon runs at
//! startup. The result is identical to the daemon's startup log line
//! (`private mesh enabled fingerprint=…`), so a UI/operator can match
//! the two without an RPC round-trip. The Argon2id cost is paid here
//! synchronously — typically ~100 ms with the production defaults.

use std::path::PathBuf;

use anyhow::{Context, Result};
use pim_core::{Config, MeshMode};

/// Plain-data view of the local node's mesh membership. Held by
/// [`cmd_mesh_status`] for printing and by the test suite for shape
/// assertions without having to capture stdout.
#[derive(Debug)]
pub(crate) struct MeshStatusReport {
    pub mode: MeshMode,
    pub mesh_id: Option<String>,
    /// `None` for open mesh; `Some(hex)` for private. The hex value
    /// matches `pim_crypto::MeshSecret::fingerprint_hex` and the
    /// daemon's startup log line.
    pub fingerprint_hex: Option<String>,
}

pub(crate) fn build_mesh_status_report(config: &Config) -> Result<MeshStatusReport> {
    match config.mesh.mode {
        MeshMode::Open => Ok(MeshStatusReport {
            mode: MeshMode::Open,
            mesh_id: config
                .mesh
                .mesh_id
                .clone()
                .filter(|s: &String| !s.is_empty()),
            fingerprint_hex: None,
        }),
        MeshMode::Private => {
            let passphrase = config
                .mesh
                .passphrase
                .as_deref()
                .filter(|s| !s.is_empty())
                .context("[mesh] mode = \"private\" but passphrase is unset; the daemon would refuse to start")?;
            let kdf = pim_crypto::MeshKdfParams {
                m_cost_kib: config.mesh.kdf.m_cost_kib,
                t_cost: config.mesh.kdf.t_cost,
                p_cost: config.mesh.kdf.p_cost,
            };
            let secret =
                pim_crypto::MeshSecret::derive(passphrase, config.mesh.mesh_id.as_deref(), kdf)
                    .context("derive mesh fingerprint from passphrase")?;
            Ok(MeshStatusReport {
                mode: MeshMode::Private,
                mesh_id: config.mesh.mesh_id.clone(),
                fingerprint_hex: Some(secret.fingerprint_hex()),
            })
        }
    }
}

pub(crate) fn cmd_mesh_status(config_path: PathBuf) -> Result<()> {
    let body = std::fs::read_to_string(&config_path)
        .with_context(|| format!("read config {}", config_path.display()))?;
    let config: Config =
        toml::from_str(&body).with_context(|| format!("parse config {}", config_path.display()))?;
    let report = build_mesh_status_report(&config)?;
    match report.mode {
        MeshMode::Open => {
            println!("mesh: open");
            if let Some(id) = &report.mesh_id {
                println!("  mesh_id (cosmetic, no protocol effect): {id}");
            }
        }
        MeshMode::Private => {
            println!("mesh: private");
            println!(
                "  mesh_id:     {}",
                report.mesh_id.as_deref().unwrap_or("(unnamed)")
            );
            println!(
                "  fingerprint: {}",
                report
                    .fingerprint_hex
                    .as_deref()
                    .expect("private mesh must produce a fingerprint")
            );
        }
    }
    Ok(())
}
