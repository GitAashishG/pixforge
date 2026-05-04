//! Production implementations of the setup traits, wiring `dialoguer`,
//! real filesystem IO, real shell rc detection, and real provider calls.

use anyhow::{anyhow, Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::traits::{
    AppendOutcome, ConfigStore, ConnectionTester, Prompter, ShellRcWriter, TestOutcome,
};
use crate::config::Profile;
use crate::providers::{ImageProvider, Request, Size};

// ---------------------------------------------------------------------------
// Prompter: dialoguer
// ---------------------------------------------------------------------------

pub struct DialoguerPrompter {
    theme: ColorfulTheme,
}

impl Default for DialoguerPrompter {
    fn default() -> Self {
        Self {
            theme: ColorfulTheme::default(),
        }
    }
}

impl Prompter for DialoguerPrompter {
    fn ask_text(&mut self, label: &str, default: Option<&str>) -> Result<String> {
        let mut input = Input::<String>::with_theme(&self.theme).with_prompt(label);
        if let Some(d) = default {
            input = input.default(d.to_string());
        }
        input.interact_text().context("reading text input")
    }

    fn ask_choice(&mut self, label: &str, choices: &[String]) -> Result<usize> {
        Select::with_theme(&self.theme)
            .with_prompt(label)
            .items(choices)
            .default(0)
            .interact()
            .context("reading menu choice")
    }

    fn confirm(&mut self, label: &str, default: bool) -> Result<bool> {
        Confirm::with_theme(&self.theme)
            .with_prompt(label)
            .default(default)
            .interact()
            .context("reading confirmation")
    }

    fn ask_secret(&mut self, label: &str) -> Result<String> {
        let prompt = format!("{label}: ");
        rpassword::prompt_password(&prompt).context("reading secret input")
    }

    fn info(&mut self, msg: &str) {
        eprintln!("  {msg}");
    }

    fn note(&mut self, msg: &str) {
        eprintln!();
        for line in msg.lines() {
            eprintln!("  {line}");
        }
        eprintln!();
    }
}

// ---------------------------------------------------------------------------
// ConfigStore: real filesystem at $XDG_CONFIG_HOME/pixforge/config.toml
// ---------------------------------------------------------------------------

pub struct FsConfigStore {
    path: PathBuf,
}

impl FsConfigStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl ConfigStore for FsConfigStore {
    fn read(&self) -> Result<Option<String>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        Ok(Some(text))
    }

    fn write(&self, contents: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
        let tmp = with_extra_extension(&self.path, "tmp");
        {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .with_context(|| format!("opening temp file {}", tmp.display()))?;
            f.write_all(contents.as_bytes())
                .with_context(|| format!("writing temp file {}", tmp.display()))?;
            f.sync_all().ok();
        }
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), self.path.display()))?;
        Ok(())
    }

    fn path(&self) -> PathBuf {
        self.path.clone()
    }
}

fn with_extra_extension(path: &Path, extra: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(extra);
    PathBuf::from(s)
}

// ---------------------------------------------------------------------------
// ShellRcWriter: real shell detection + idempotent append
// ---------------------------------------------------------------------------

pub struct FsShellRcWriter;

impl FsShellRcWriter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FsShellRcWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellRcWriter for FsShellRcWriter {
    fn rc_path(&self) -> Option<PathBuf> {
        detect_shell_rc()
    }

    fn append_export(&self, var: &str, val: &str) -> Result<AppendOutcome> {
        let path = self
            .rc_path()
            .ok_or_else(|| anyhow!("could not determine shell rc file (set $SHELL?)"))?;

        let existing = if path.exists() {
            std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
        } else {
            String::new()
        };

        // Idempotent: skip if a line already exports this var (any value).
        let needle = format!("export {var}=");
        if existing
            .lines()
            .any(|l| l.trim_start().starts_with(&needle))
        {
            return Ok(AppendOutcome::AlreadyPresent { path });
        }

        // Backup before write (only if file already had content).
        if !existing.is_empty() {
            let bak = with_extra_extension(&path, "bak");
            std::fs::write(&bak, &existing)
                .with_context(|| format!("writing backup {}", bak.display()))?;
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        // Quote the value: simple shell-safe single-quote escaping.
        let escaped = val.replace('\'', r"'\''");
        let mut new_content = existing;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(&format!("\n# pixforge: {var}\nexport {var}='{escaped}'\n"));

        let tmp = with_extra_extension(&path, "tmp");
        std::fs::write(&tmp, &new_content)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;

        Ok(AppendOutcome::Appended { path })
    }
}

/// Resolve the user's shell rc file from `$SHELL`. Returns `None` for
/// unrecognized shells so the wizard can offer the "print export, don't
/// edit" path instead.
fn detect_shell_rc() -> Option<PathBuf> {
    let shell = env::var("SHELL").ok()?;
    let home = dirs::home_dir()?;
    let basename = shell.rsplit('/').next().unwrap_or(&shell);
    match basename {
        "zsh" => Some(home.join(".zshrc")),
        "bash" => {
            // macOS users typically have ~/.bash_profile; Linux has ~/.bashrc.
            // Prefer whichever already exists; fall back to platform default.
            let bp = home.join(".bash_profile");
            let br = home.join(".bashrc");
            if bp.exists() {
                Some(bp)
            } else if br.exists() {
                Some(br)
            } else if cfg!(target_os = "macos") {
                Some(bp)
            } else {
                Some(br)
            }
        }
        "fish" => Some(home.join(".config").join("fish").join("config.fish")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// ConnectionTester: real provider call
// ---------------------------------------------------------------------------

pub struct LiveConnectionTester {
    pub builder: Box<dyn Fn(&Profile) -> Result<Box<dyn ImageProvider>> + Send + Sync>,
}

impl ConnectionTester for LiveConnectionTester {
    fn test(&mut self, profile: &Profile) -> Result<TestOutcome> {
        let provider = (self.builder)(profile)?;
        let extra = serde_json::Map::new();
        let size = Some(Size {
            width: profile.width,
            height: profile.height,
        });
        let req = Request {
            prompt: "small test image, simple",
            model: &profile.model,
            n: 1,
            size,
            size_explicit: false,
            seed: None,
            negative_prompt: None,
            quality: Some("low"),
            extra: &extra,
        };
        let mut on_retry = |attempt: u32, msg: &str, wait: f64| {
            eprintln!("  retry {attempt}: {msg}; sleeping {wait:.1}s")
        };
        let result = provider
            .generate(&req, &mut on_retry)
            .context("test generation failed")?;
        let bytes = result.images.first().map(|i| i.bytes.len()).unwrap_or(0);
        Ok(TestOutcome {
            bytes,
            latency_secs: result.latency_secs,
            attempts: result.attempts,
        })
    }
}
