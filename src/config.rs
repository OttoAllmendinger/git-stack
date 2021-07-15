use eyre::WrapErr;

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RepoConfig {
    pub protected_branches: Option<Vec<String>>,
}

static PROTECTED_BRANCH_FIELD: &str = "stack.protected-branch";
static DEFAULT_PROTECTED_BRANCHES: [&str; 4] = ["/main", "/master", "/dev", "/stable"];

impl RepoConfig {
    pub fn from_all(repo: &git2::Repository) -> eyre::Result<Self> {
        let config = Self::from_defaults();
        let config = config.merge(Self::from_workdir(repo)?);
        let config = config.merge(Self::from_repo(repo)?);
        Ok(config)
    }

    pub fn from_repo(repo: &git2::Repository) -> eyre::Result<Self> {
        let workdir = repo
            .workdir()
            .ok_or_else(|| eyre::eyre!("Cannot read config in bare repository."))?;
        let config_path = workdir.join(".git/config");
        log::debug!("Loading {}", config_path.display());
        if config_path.exists() {
            match git2::Config::open(&config_path) {
                Ok(config) => Ok(Self::from_gitconfig(&config)),
                Err(err) => {
                    log::debug!("Failed to load git config: {}", err);
                    Ok(Default::default())
                }
            }
        } else {
            Ok(Default::default())
        }
    }

    pub fn from_workdir(repo: &git2::Repository) -> eyre::Result<Self> {
        let workdir = repo
            .workdir()
            .ok_or_else(|| eyre::eyre!("Cannot read config in bare repository."))?;
        let config_path = workdir.join(".git-stack.toml");
        log::debug!("Loading {}", config_path.display());
        if config_path.exists() {
            Self::from_path(&config_path)
        } else {
            Ok(Default::default())
        }
    }

    pub fn from_defaults() -> Self {
        let mut protected_branches: Vec<String> = Vec::new();

        log::debug!("Loading gitconfig");
        match git2::Config::open_default() {
            Ok(config) => {
                let default_branch = crate::git::default_branch(&config);
                let default_branch_ignore = format!("/{}", default_branch);
                protected_branches.push(default_branch_ignore);
            }
            Err(err) => {
                log::debug!("Failed to load git config: {}", err);
            }
        }
        // Don't bother with removing duplicates if `default_branch` is the same as one of our
        // default protected branches
        protected_branches.extend(DEFAULT_PROTECTED_BRANCHES.iter().map(|s| (*s).to_owned()));

        Self {
            protected_branches: Some(protected_branches),
        }
    }

    pub fn from_path(path: &std::path::Path) -> eyre::Result<Self> {
        let s = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("Could not read config from {}.", path.display()))?;
        Self::from_toml(&s)
            .wrap_err_with(|| format!("Could not read config from {}.", path.display()))
    }

    pub fn from_toml(data: &str) -> eyre::Result<Self> {
        let config = toml::from_str(data)?;
        Ok(config)
    }

    pub fn from_gitconfig(config: &git2::Config) -> Self {
        let protected_branches = config
            .multivar(PROTECTED_BRANCH_FIELD, None)
            .map(|entries| {
                let entries_ref = &entries;
                let protected_branches: Vec<_> = entries_ref
                    .flat_map(|e| e.into_iter())
                    .filter_map(|e| e.value().map(|v| v.to_owned()))
                    .collect();
                if protected_branches.is_empty() {
                    None
                } else {
                    Some(protected_branches)
                }
            })
            .unwrap_or(None);

        Self {
            protected_branches: protected_branches,
        }
    }

    pub fn write_repo(&self, repo: &git2::Repository) -> eyre::Result<()> {
        let workdir = repo
            .workdir()
            .ok_or_else(|| eyre::eyre!("Cannot read config in bare repository."))?;
        let config_path = workdir.join(".git/config");
        log::debug!("Loading {}", config_path.display());
        let mut config = git2::Config::open(&config_path)?;
        log::info!("Writing {}", config_path.display());
        self.to_gitconfig(&mut config)?;
        Ok(())
    }

    pub fn to_gitconfig(&self, config: &mut git2::Config) -> eyre::Result<()> {
        if let Some(protected_branches) = self.protected_branches.as_ref() {
            // Ignore errors if there aren't keys to remove
            let _ = config.remove_multivar(PROTECTED_BRANCH_FIELD, ".*");
            for branch in protected_branches {
                config.set_multivar(PROTECTED_BRANCH_FIELD, "^$", branch)?;
            }
        }
        Ok(())
    }

    pub fn merge(mut self, other: Self) -> Self {
        match (&mut self.protected_branches, other.protected_branches) {
            (Some(lhs), Some(rhs)) => lhs.extend(rhs),
            (None, Some(rhs)) => self.protected_branches = Some(rhs),
            (_, _) => (),
        }
        self
    }
}