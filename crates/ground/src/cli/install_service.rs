use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct InstallServiceOptions {
    /// Absolute path of the `groundd` binary.
    pub daemon_entry: PathBuf,
    /// Base for the user unit directory; default `~/.config`.
    pub config_home: Option<PathBuf>,
    /// Written into the unit only when it is not the default.
    pub ground_home: Option<PathBuf>,
}

fn quote(arg: &str) -> String {
    if arg
        .chars()
        .any(|c| c.is_whitespace() || "\"'\\$%".contains(c))
    {
        let escaped = arg
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%");
        format!("\"{escaped}\"")
    } else {
        arg.to_owned()
    }
}

pub fn render_unit(daemon_entry: &Path, ground_home: Option<&Path>) -> String {
    let env = ground_home
        .map(|h| {
            format!(
                "Environment={}\n",
                quote(&format!("GROUND_HOME={}", h.display()))
            )
        })
        .unwrap_or_default();
    format!(
        "[Unit]\n\
         Description=ground: drives merge, validation and release\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={}\n\
         {env}Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        quote(&daemon_entry.display().to_string())
    )
}

/// The `groundd` binary next to the running `ground`.
pub fn built_daemon_entry() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate ground: {e}"))?;
    let entry = exe.with_file_name("groundd");
    if entry.is_file() {
        Ok(entry)
    } else {
        Err(format!(
            "install-service needs groundd next to ground ({} is missing)",
            entry.display()
        ))
    }
}

/// Writes the unit and returns its path with the command that would enable it.
/// Never runs systemctl.
pub fn install_service(opts: &InstallServiceOptions) -> std::io::Result<(PathBuf, String)> {
    let base = match &opts.config_home {
        Some(c) => c.clone(),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    let dir = base.join("systemd").join("user");
    std::fs::create_dir_all(&dir)?;
    let unit_path = dir.join("groundd.service");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&unit_path)?;
    f.write_all(render_unit(&opts.daemon_entry, opts.ground_home.as_deref()).as_bytes())?;
    Ok((
        unit_path,
        "systemctl --user daemon-reload && systemctl --user enable --now groundd.service".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_unit_under_config_home_and_returns_the_enable_command() {
        let dir = tempfile::tempdir().unwrap();
        let (path, command) = install_service(&InstallServiceOptions {
            daemon_entry: "/opt/ground/bin/groundd".into(),
            config_home: Some(dir.path().to_owned()),
            ground_home: None,
        })
        .unwrap();
        assert_eq!(path, dir.path().join("systemd/user/groundd.service"));
        assert!(path.is_file());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("ExecStart=/opt/ground/bin/groundd\n"));
        assert!(text.contains("WantedBy=default.target"));
        assert!(!text.contains("Environment="));
        assert!(command.contains("systemctl --user enable --now groundd.service"));
    }

    #[test]
    fn quotes_paths_with_spaces_and_carries_a_custom_ground_home() {
        let text = render_unit(
            Path::new("/home/a b/bin/groundd"),
            Some(Path::new("/data/ground")),
        );
        assert!(text.contains("ExecStart=\"/home/a b/bin/groundd\""));
        assert!(text.contains("Environment=GROUND_HOME=/data/ground"));
    }
}
