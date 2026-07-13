//! `gw setup [--remove]`: install hooks for every discovered plugin and
//! report per-file outcomes.

use anyhow::Result;

pub fn run(remove: bool) -> Result<()> {
    let plugins = gw_core::plugins::discover()?;
    let without_hooks = plugins
        .iter()
        .filter(|plugin| plugin.manifest.hooks.is_empty())
        .map(|plugin| plugin.manifest.id.as_str())
        .collect::<Vec<_>>();
    if !without_hooks.is_empty() {
        eprintln!(
            "warning: providers with no hooks: {}",
            without_hooks.join(", ")
        );
    }

    let manifests = plugins
        .iter()
        .map(|plugin| plugin.manifest.clone())
        .collect::<Vec<_>>();
    let outcomes = if remove {
        gw_core::setup::remove(&manifests)?
    } else {
        gw_core::setup::install(&manifests)?
    };
    for (path, outcome) in outcomes {
        let label = match outcome {
            gw_core::setup::Outcome::Changed => "Changed",
            gw_core::setup::Outcome::AlreadyApplied => "AlreadyApplied",
        };
        println!("{}: {label}", path.display());
    }
    Ok(())
}
