//! Запомненные отпечатки сертификатов BMC: `known_hosts.json` в каталоге данных приложения.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct KnownHosts {
    path: PathBuf,
    hosts: BTreeMap<String, String>,
}

impl KnownHosts {
    pub fn load(path: PathBuf) -> Self {
        let hosts = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { path, hosts }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.hosts.get(key).cloned()
    }

    pub fn set(&mut self, key: String, fingerprint: String) -> std::io::Result<()> {
        if self.hosts.get(&key) == Some(&fingerprint) {
            return Ok(());
        }
        self.hosts.insert(key, fingerprint);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(&self.hosts).expect("строки сериализуются");
        std::fs::write(&self.path, json)
    }
}
