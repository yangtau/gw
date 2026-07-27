//! Manifest command-template expansion. The core owns placeholder expansion
//! (`{session_id}`, `{prompt}`, `{cwd}`); plugins only declare templates.

use std::path::Path;

/// Expand a manifest command template. `{prompt}` arguments are dropped
/// entirely when no prompt is given, so one template serves both cases.
pub fn expand_argv(
    argv: &[String],
    session_id: &str,
    prompt: Option<&str>,
    cwd: &Path,
) -> Vec<String> {
    argv.iter()
        .filter(|arg| prompt.is_some() || !arg.contains("{prompt}"))
        .map(|arg| {
            arg.replace("{session_id}", session_id)
                .replace("{prompt}", prompt.unwrap_or_default())
                .replace("{cwd}", &cwd.to_string_lossy())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_placeholders() {
        let argv = ["claude", "--resume", "{session_id}", "{prompt}"]
            .map(str::to_owned)
            .to_vec();
        assert_eq!(
            expand_argv(&argv, "s1", Some("continue the work"), Path::new("/w")),
            ["claude", "--resume", "s1", "continue the work"]
        );
    }

    #[test]
    fn drops_prompt_arguments_without_a_prompt() {
        let argv = ["codex", "resume", "{session_id}", "{prompt}"]
            .map(str::to_owned)
            .to_vec();
        assert_eq!(
            expand_argv(&argv, "s1", None, Path::new("/w")),
            ["codex", "resume", "s1"]
        );
    }

    #[test]
    fn expands_cwd() {
        let argv = ["run", "{cwd}"].map(str::to_owned).to_vec();
        assert_eq!(
            expand_argv(&argv, "s1", None, Path::new("/work/dir")),
            ["run", "/work/dir"]
        );
    }
}
