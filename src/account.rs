use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(serde::Serialize, serde::Deserialize, Clone, Default, Debug)]
pub struct Account {
    pub base: String,
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub csrf: Option<String>,
}

impl Account {
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }

    pub fn matches(&self, base: &str, username: &str) -> bool {
        let base = base.trim_end_matches('/');
        let other = self.base.trim_end_matches('/');
        base.eq_ignore_ascii_case(other) && self.username.eq_ignore_ascii_case(username)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct AccountStore {
    #[serde(default)]
    pub accounts: Vec<Account>,

    #[serde(default)]
    pub selected: Option<usize>,
}

impl AccountStore {
    pub fn path() -> PathBuf {
        let mut config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        config_dir.push("plainwire");
        config_dir
    }

    pub fn load() -> Self {
        let path = Self::path();
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(mut store) = serde_json::from_str::<AccountStore>(&data) {
                let len = store.accounts.len();
                if store.selected.is_some_and(|i| i >= len) {
                    store.selected = None;
                }
                return store;
            }
        }
        Self::default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::File::create(path)?;
        let json = serde_json::to_string_pretty(self)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    pub fn index_of(&self, base: &str, username: &str) -> Option<usize> {
        self.accounts.iter().position(|a| a.matches(base, username))
    }

    pub fn by_user_id(&self, user_id: i64) -> Option<&Account> {
        self.accounts.iter().find(|a| a.user_id == user_id)
    }

    pub fn upsert(&mut self, mut account: Account) -> usize {
        match self
            .accounts
            .iter()
            .position(|a| a.matches(&account.base, &account.username))
        {
            Some(index) => {
                if account.token.is_none() {
                    account.token = self.accounts[index].token.clone();
                    account.csrf = self.accounts[index].csrf.clone();
                }
                self.accounts[index] = account;
                index
            }
            None => {
                self.accounts.push(account);
                self.accounts.len() - 1
            }
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index >= self.accounts.len() {
            return;
        }
        self.accounts.remove(index);
        self.selected = match self.selected {
            Some(selected) if selected == index => None,
            Some(selected) if selected > index => Some(selected - 1),
            other => other,
        };
    }

    pub fn select(&mut self, index: usize) {
        if index < self.accounts.len() {
            self.selected = Some(index);
        }
    }

    pub fn preferred(&self) -> Option<usize> {
        self.selected.or(if self.accounts.len() == 1 {
            Some(0)
        } else {
            None
        })
    }
}
