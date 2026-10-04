use std::fs;
use std::io::Write;
use std::path::PathBuf;


#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Account {
    pub base: String,
    pub username: String,
    pub password: String,
    pub user_id: i64,
    pub display_name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct AccountStore {
    pub accounts: Vec<Account>,
}

impl AccountStore {
    pub fn path() -> PathBuf {
        let mut config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."));
        config_dir.push("plainwire");
        config_dir
    }

    pub fn load() -> Self {
        let path = Self::path();
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(store) = serde_json::from_str(&data) {
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
}
