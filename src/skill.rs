//! `deaddrop skill`: install an Agent Skill (https://agentskills.io) that
//! teaches AI coding agents how to use and configure deaddrop.
//!
//! The skill is a folder named `deaddrop` holding `SKILL.md` and
//! `references/config.md`, both built into the binary so they always match
//! this version. It can go into each harness's global (per user) or project
//! skills directory:
//!
//! | Harness | Global              | Project           |
//! |---------|---------------------|-------------------|
//! | claude  | `~/.claude/skills`  | `.claude/skills`  |
//! | codex   | `~/.agents/skills`  | `.agents/skills`  |
//! | copilot | `~/.copilot/skills` | `.github/skills`  |
//! | agents  | `~/.agents/skills`  | `.agents/skills`  |
//!
//! `agents` is the cross-tool location from the Agent Skills standard (read by
//! Codex, the Copilot CLI and others); it is the same directory as `codex`.
//! Files are only replaced or removed if deaddrop wrote them, unless
//! `--force` is given.

use std::{
    collections::BTreeMap,
    io::{BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::write_stdout;

/// The skill's folder name, which the spec requires to match `name:`.
const NAME: &str = "deaddrop";
// The skill is committed under skills/deaddrop/ so tools can also install it
// straight from the repository. Tests check it matches this build.
const SKILL_MD: &str = include_str!("../skills/deaddrop/SKILL.md");
const CONFIG_MD: &str = include_str!("../skills/deaddrop/references/config.md");
/// Marks files deaddrop wrote, so updates and uninstalls never touch a
/// skill someone else put there.
const MARKER: &str = "generated-by: deaddrop";
const CONFIG_REFERENCE: &str = "references/config.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum Harness {
    /// Claude Code
    Claude,
    /// OpenAI Codex
    Codex,
    /// GitHub Copilot (VS Code, CLI, cloud agent)
    Copilot,
    /// The shared Agent Skills location (Codex, Copilot CLI and others)
    Agents,
}

impl Harness {
    const ALL: [Harness; 4] = [Self::Claude, Self::Codex, Self::Copilot, Self::Agents];

    fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "OpenAI Codex",
            Self::Copilot => "GitHub Copilot",
            Self::Agents => "Agent Skills standard (.agents)",
        }
    }

    /// Skills directory, relative to the home dir (global) or the project
    /// root (project).
    fn skills_dir(self, scope: Scope) -> &'static str {
        match (self, scope) {
            (Self::Claude, _) => ".claude/skills",
            (Self::Codex | Self::Agents, _) => ".agents/skills",
            (Self::Copilot, Scope::Global) => ".copilot/skills",
            (Self::Copilot, Scope::Project) => ".github/skills",
        }
    }

    /// A directory whose presence suggests this harness is in use there.
    fn marker_dir(self, scope: Scope) -> &'static str {
        match (self, scope) {
            (Self::Claude, _) => ".claude",
            (Self::Codex, Scope::Global) => ".codex",
            (Self::Codex | Self::Agents, _) => ".agents",
            (Self::Copilot, Scope::Global) => ".copilot",
            (Self::Copilot, Scope::Project) => ".github",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// For the current user, in every project.
    Global,
    /// For one project; commit it to share with the team.
    Project,
}

/// What `install` and `uninstall` act on.
pub struct Target {
    /// `None`: ask, or fail if there's no terminal.
    pub scope: Option<Scope>,
    /// Empty: ask, or fail if there's no terminal.
    pub harnesses: Vec<Harness>,
    /// Project root. Defaults to the enclosing git repository, else the
    /// current directory.
    pub dir: Option<PathBuf>,
}

/// The files of the skill, by path inside the skill folder.
fn files() -> Vec<(&'static str, String)> {
    vec![
        ("SKILL.md", SKILL_MD.to_string()),
        (CONFIG_REFERENCE, CONFIG_MD.to_string()),
    ]
}

/// What `references/config.md` must contain: the config template of this
/// build.
#[cfg(test)]
fn config_reference() -> String {
    format!(
        "# deaddrop config file reference\n\n\
         Every setting deaddrop reads from its TOML config file, as printed by \
         `deaddrop config example`. Every key is optional. Precedence: \
         command-line flag > environment variable > config file > default. \
         Validate with `deaddrop config check`.\n\n\
         <!-- {MARKER} -->\n\n```toml\n{}```\n",
        crate::config::EXAMPLE
    )
}

/// `deaddrop skill show`
pub fn show() -> Result<()> {
    let files = files();
    write_stdout(&files[0].1)
}

/// `deaddrop skill install`
pub fn install(target: Target, force: bool) -> Result<()> {
    let plan = resolve(target, "install")?;
    let files = files();
    let mut installed = 0;
    for (dir, harnesses) in plan {
        let skill = dir.join(NAME);
        let who = names(&harnesses);
        match state(&skill, &files)? {
            State::Current => {
                eprintln!("{who}: already up to date in {}", skill.display());
                continue;
            }
            State::Foreign if !force => {
                eprintln!(
                    "{who}: skipped {}: a `{NAME}` skill not written by deaddrop is already \
                     there (use --force to replace it)",
                    skill.display()
                );
                continue;
            }
            State::Absent | State::Outdated | State::Foreign => {}
        }
        for (rel, content) in &files {
            let path = skill.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::write(&path, content)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        eprintln!("{who}: installed {}", skill.join("SKILL.md").display());
        installed += 1;
    }
    if installed > 0 {
        eprintln!(
            "restart running agents (or reload their skills) to pick it up. Ask them to \
         \"set up a deaddrop config file\" or to fetch a ticket."
        );
    }
    Ok(())
}

/// `deaddrop skill uninstall`
pub fn uninstall(target: Target, force: bool) -> Result<()> {
    let plan = resolve(target, "uninstall")?;
    let files = files();
    for (dir, harnesses) in plan {
        let skill = dir.join(NAME);
        let who = names(&harnesses);
        match state(&skill, &files)? {
            State::Absent => {
                eprintln!("{who}: not installed in {}", dir.display());
                continue;
            }
            State::Foreign if !force => {
                eprintln!(
                    "{who}: left {} alone: it wasn't written by deaddrop (use --force to \
                     remove it anyway)",
                    skill.display()
                );
                continue;
            }
            _ => {}
        }
        // Remove only our files, then the folders if that left them empty,
        // so anything the user added survives.
        for (rel, _) in &files {
            let path = skill.join(rel);
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("failed to remove {}", path.display()));
                }
                _ => {}
            }
        }
        std::fs::remove_dir(skill.join("references")).ok();
        let gone = std::fs::remove_dir(&skill).is_ok();
        if gone {
            eprintln!("{who}: removed {}", skill.display());
        } else {
            eprintln!(
                "{who}: removed the skill files; kept {} because it has other files in it",
                skill.display()
            );
        }
    }
    Ok(())
}

/// `deaddrop skill status`: where the skill is installed, for every harness
/// and both scopes.
pub fn status(dir: Option<PathBuf>) -> Result<()> {
    let files = files();
    let home = home()?;
    let project = project_root(dir)?;
    let mut out = String::new();
    for (scope, base, title) in [
        (Scope::Global, &home, "global".to_string()),
        (
            Scope::Project,
            &project,
            format!("project ({})", project.display()),
        ),
    ] {
        out.push_str(&format!("{title}:\n"));
        for (dir, harnesses) in group(base, scope, &Harness::ALL) {
            let skill = dir.join(NAME);
            let state = match state(&skill, &files)? {
                State::Absent => "not installed",
                State::Current => "installed, up to date",
                State::Outdated => "installed, outdated (run `deaddrop skill install` again)",
                State::Foreign => "a different `deaddrop` skill (not written by deaddrop)",
            };
            out.push_str(&format!(
                "  {:<15} {state}\n  {:<15} {}\n",
                names(&harnesses),
                "",
                skill.display()
            ));
        }
    }
    write_stdout(&out)
}

#[derive(Debug, PartialEq)]
enum State {
    Absent,
    /// Ours, with the same content as this version.
    Current,
    /// Ours, from another version.
    Outdated,
    /// A `deaddrop` skill folder we didn't write.
    Foreign,
}

fn state(skill: &Path, files: &[(&str, String)]) -> Result<State> {
    let skill_md = skill.join("SKILL.md");
    let existing = match std::fs::read_to_string(&skill_md) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // An empty or unrelated folder of that name isn't ours.
            return Ok(if skill.exists() {
                State::Foreign
            } else {
                State::Absent
            });
        }
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", skill_md.display())),
    };
    if !existing.contains(MARKER) {
        return Ok(State::Foreign);
    }
    let same = files.iter().all(|(rel, content)| {
        std::fs::read_to_string(skill.join(rel)).is_ok_and(|s| s == *content)
    });
    Ok(if same {
        State::Current
    } else {
        State::Outdated
    })
}

/// Turn the requested target into skills directories, each with the
/// harnesses that read it (`codex` and `agents` share one).
fn resolve(target: Target, verb: &str) -> Result<BTreeMap<PathBuf, Vec<Harness>>> {
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let project = project_root(target.dir)?;
    let home = home()?;

    let scope = match target.scope {
        Some(s) => s,
        None if interactive => ask_scope(&project, verb)?,
        None => bail!(
            "choose where to {verb} the skill: --global (for your user, in every project) or \
             --project (for {})",
            project.display()
        ),
    };
    let base = match scope {
        Scope::Global => &home,
        Scope::Project => &project,
    };
    let harnesses = if !target.harnesses.is_empty() {
        target.harnesses
    } else if interactive {
        ask_harnesses(base, scope, verb)?
    } else {
        bail!(
            "choose which agents to {verb} the skill for: --harness claude,codex,copilot,agents \
             (or --all)"
        )
    };
    Ok(group(base, scope, &harnesses))
}

fn group(base: &Path, scope: Scope, harnesses: &[Harness]) -> BTreeMap<PathBuf, Vec<Harness>> {
    let mut plan: BTreeMap<PathBuf, Vec<Harness>> = BTreeMap::new();
    for &h in harnesses {
        let entry = plan.entry(base.join(h.skills_dir(scope))).or_default();
        if !entry.contains(&h) {
            entry.push(h);
        }
    }
    plan
}

fn names(harnesses: &[Harness]) -> String {
    harnesses
        .iter()
        .map(|h| {
            h.to_possible_value()
                .expect("no skipped variants")
                .get_name()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn home() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .context("can't find your home directory")
}

/// `--dir`, else the enclosing git repository, else the current directory.
fn project_root(dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(d) = dir {
        return std::path::absolute(&d).with_context(|| format!("bad directory {}", d.display()));
    }
    let cwd = std::env::current_dir().context("can't read the current directory")?;
    Ok(cwd
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(&cwd)
        .to_path_buf())
}

fn ask(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let n = std::io::stdin().lock().read_line(&mut line)?;
    if n == 0 {
        bail!("no answer (end of input)");
    }
    Ok(line.trim().to_lowercase())
}

fn ask_scope(project: &Path, verb: &str) -> Result<Scope> {
    eprintln!("Where should deaddrop {verb} the agent skill?");
    eprintln!("  1) global   for your user, in every project");
    eprintln!(
        "  2) project  only in {} (commit it to share it)",
        project.display()
    );
    loop {
        match ask("choose 1 or 2 [1]: ")?.as_str() {
            "" | "1" | "g" | "global" => return Ok(Scope::Global),
            "2" | "p" | "project" => return Ok(Scope::Project),
            _ => eprintln!("please answer 1 or 2"),
        }
    }
}

fn ask_harnesses(base: &Path, scope: Scope, verb: &str) -> Result<Vec<Harness>> {
    let detected: Vec<Harness> = Harness::ALL
        .into_iter()
        .filter(|h| base.join(h.marker_dir(scope)).exists())
        .collect();
    eprintln!("Which agents should deaddrop {verb} it for?");
    for (i, h) in Harness::ALL.iter().enumerate() {
        let found = if detected.contains(h) {
            "  (found)"
        } else {
            ""
        };
        eprintln!(
            "  {}) {:<8} {:<32} {}{found}",
            i + 1,
            names(&[*h]),
            h.label(),
            base.join(h.skills_dir(scope)).display()
        );
    }
    let default = if detected.is_empty() {
        "all".to_string()
    } else {
        detected
            .iter()
            .map(|h| (Harness::ALL.iter().position(|x| x == h).expect("in ALL") + 1).to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    loop {
        let answer = ask(&format!(
            "numbers or names, comma-separated, or `all` [{default}]: "
        ))?;
        let answer = if answer.is_empty() {
            default.clone()
        } else {
            answer
        };
        match parse_choice(&answer) {
            Ok(v) => return Ok(v),
            Err(e) => eprintln!("{e}"),
        }
    }
}

fn parse_choice(answer: &str) -> Result<Vec<Harness>> {
    if answer == "all" || answer == "a" {
        return Ok(Harness::ALL.to_vec());
    }
    let mut out = Vec::new();
    for part in answer.split([',', ' ']).filter(|p| !p.is_empty()) {
        let h = match part.parse::<usize>() {
            Ok(n) if (1..=Harness::ALL.len()).contains(&n) => Harness::ALL[n - 1],
            Ok(_) => bail!("{part} isn't one of the numbers listed"),
            Err(_) => Harness::from_str(part, true)
                .map_err(|_| anyhow::anyhow!("unknown agent {part:?}"))?,
        };
        if !out.contains(&h) {
            out.push(h);
        }
    }
    if out.is_empty() {
        bail!("choose at least one");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_follows_the_spec() {
        let files = files();
        let md = &files[0].1;
        let front = md
            .strip_prefix("---\n")
            .and_then(|s| s.split_once("\n---\n"))
            .expect("frontmatter")
            .0;
        assert!(front.lines().any(|l| l == format!("name: {NAME}")));
        let desc = front
            .lines()
            .find_map(|l| l.strip_prefix("description: "))
            .expect("description");
        assert!(
            !desc.is_empty() && desc.len() <= 1024,
            "description is {} chars",
            desc.len()
        );
        assert!(front.contains(MARKER));
        assert!(
            front.contains(&format!("version: \"{}\"", env!("CARGO_PKG_VERSION"))),
            "update `version:` in skills/deaddrop/SKILL.md to match Cargo.toml"
        );
        assert!(md.contains("references/config.md"));
        assert!(md.lines().count() < 500);
        assert!(files[1].1.contains("[serve]") && files[1].1.contains(MARKER));
    }

    /// `references/config.md` is generated from config.example.toml. Run
    /// `DEADDROP_UPDATE_SKILL=1 cargo test` to rewrite it after changing the
    /// template.
    #[test]
    fn config_reference_is_current() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/deaddrop/references/config.md");
        let want = config_reference();
        if std::env::var_os("DEADDROP_UPDATE_SKILL").is_some() {
            std::fs::write(&path, &want).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            want,
            "skills/deaddrop/references/config.md is out of date: run `DEADDROP_UPDATE_SKILL=1 cargo test`"
        );
    }

    #[test]
    fn codex_and_agents_share_a_directory() {
        let plan = group(Path::new("/h"), Scope::Global, &Harness::ALL);
        assert_eq!(plan.len(), 3);
        assert_eq!(
            plan[Path::new("/h/.agents/skills")],
            vec![Harness::Codex, Harness::Agents]
        );
        let project = group(Path::new("/p"), Scope::Project, &[Harness::Copilot]);
        assert!(project.contains_key(Path::new("/p/.github/skills")));
    }

    #[test]
    fn choices() {
        assert_eq!(parse_choice("all").unwrap(), Harness::ALL.to_vec());
        assert_eq!(
            parse_choice("1, copilot,1").unwrap(),
            vec![Harness::Claude, Harness::Copilot]
        );
        assert!(parse_choice("9").is_err());
        assert!(parse_choice("vim").is_err());
    }

    #[test]
    fn install_update_uninstall() {
        let dir = std::env::temp_dir().join(format!("deaddrop-skill-test-{}", std::process::id()));
        let skill = dir.join(NAME);
        let files = files();
        assert_eq!(state(&skill, &files).unwrap(), State::Absent);

        // Someone else's skill of the same name.
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: deaddrop\n---\nmine").unwrap();
        assert_eq!(state(&skill, &files).unwrap(), State::Foreign);

        // Ours, from another version.
        std::fs::write(skill.join("SKILL.md"), format!("old\n{MARKER}\n")).unwrap();
        assert_eq!(state(&skill, &files).unwrap(), State::Outdated);

        for (rel, content) in &files {
            let p = skill.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        assert_eq!(state(&skill, &files).unwrap(), State::Current);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
