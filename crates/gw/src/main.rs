use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use gw_core::config::{Config, PanelView};

mod hook;
mod setup;
mod tui;

#[derive(Parser)]
#[command(name = "gw", about = "Coding agent panel for tmux")]
struct Cli {
    /// Initial agent view. Overrides [panel].default_view in the config.
    #[arg(long, value_enum)]
    view: Option<ViewArg>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the panel (default).
    Panel {
        /// Initial agent view. Overrides [panel].default_view in the config.
        #[arg(long, value_enum)]
        view: Option<ViewArg>,
    },
    /// Ingest one hook payload from stdin for the given provider.
    Hook { provider: String },
    /// Install hooks into provider configs (global).
    Setup {
        /// Remove previously installed hooks instead.
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ViewArg {
    Current,
    Global,
}

impl From<ViewArg> for PanelView {
    fn from(value: ViewArg) -> Self {
        match value {
            ViewArg::Current => Self::Current,
            ViewArg::Global => Self::Global,
        }
    }
}

fn resolve_panel_view(cli_view: Option<ViewArg>, config_view: PanelView) -> PanelView {
    cli_view.map(PanelView::from).unwrap_or(config_view)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Panel { view }) => {
            let config = Config::load();
            tui::run(resolve_panel_view(
                view.or(cli.view),
                config.panel.default_view,
            ))
        }
        Some(Cmd::Hook { provider }) => hook::run(&provider),
        Some(Cmd::Setup { remove }) => setup::run(remove),
        None => {
            let config = Config::load();
            tui::run(resolve_panel_view(cli.view, config.panel.default_view))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_view_overrides_config() {
        assert_eq!(
            resolve_panel_view(Some(ViewArg::Global), PanelView::Current),
            PanelView::Global
        );
        assert_eq!(
            resolve_panel_view(Some(ViewArg::Current), PanelView::Global),
            PanelView::Current
        );
    }

    #[test]
    fn config_view_applies_without_cli_override() {
        assert_eq!(
            resolve_panel_view(None, PanelView::Global),
            PanelView::Global
        );
    }

    #[test]
    fn view_flag_is_accepted_for_default_and_explicit_panel_commands() {
        let default = Cli::try_parse_from(["gw", "--view", "global"]).unwrap();
        assert_eq!(default.view, Some(ViewArg::Global));
        assert!(default.cmd.is_none());

        let explicit = Cli::try_parse_from(["gw", "panel", "--view", "current"]).unwrap();
        assert_eq!(explicit.view, None);
        assert!(matches!(
            explicit.cmd,
            Some(Cmd::Panel {
                view: Some(ViewArg::Current)
            })
        ));

        assert!(Cli::try_parse_from(["gw", "hook", "claude", "--view", "global"]).is_err());
    }
}
