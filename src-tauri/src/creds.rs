// 凭据 vault（过渡实现）
// 桌面: app 数据目录 creds.json（明文过渡, P8 换 OS keyring/Keychain/Keystore）
// 接口按 keyring 语义设计(save/load/delete), 届时仅换实现。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct Vault {
    path: PathBuf,
    cache: Mutex<HashMap<String, String>>,
}

static VAULT: std::sync::LazyLock<Vault> = std::sync::LazyLock::new(|| {
    let dir = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("data");
    let _ = std::fs::create_dir_all(&dir);
    Vault {
        path: dir.join("creds.json"),
        cache: Mutex::new(HashMap::new()),
    }
});

fn load_all(path: &PathBuf) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(key_ref: &str, secret: &str) -> Result<(), String> {
    let v = &VAULT;
    // 文件读-改-写整体持锁，防并发丢更新（cache 与磁盘同锁）
    let _guard = v.cache.lock();
    let mut all = load_all(&v.path);
    all.insert(key_ref.to_string(), secret.to_string());
    std::fs::write(&v.path, serde_json::to_string_pretty(&all).unwrap())
        .map_err(|e| format!("写入凭据失败: {e}"))?;
    Ok(())
}

pub fn load(key_ref: &str) -> Result<String, String> {
    let v = &VAULT;
    let _guard = v.cache.lock();
    load_all(&v.path)
        .get(key_ref)
        .cloned()
        .ok_or_else(|| format!("凭据不存在: {key_ref}"))
}

pub fn delete(key_ref: &str) -> Result<(), String> {
    let v = &VAULT;
    let _guard = v.cache.lock();
    let mut all = load_all(&v.path);
    all.remove(key_ref);
    std::fs::write(&v.path, serde_json::to_string_pretty(&all).unwrap())
        .map_err(|e| format!("写入凭据失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意: vault 是进程级单例(读写 data/creds.json), 单测用独立 key 避免互扰
    #[test]
    fn save_load_delete_roundtrip() {
        let key = format!("test-key-{}", std::process::id());
        save(&key, "secret-value").unwrap();
        assert_eq!(load(&key).unwrap(), "secret-value");
        delete(&key).unwrap();
        assert!(load(&key).is_err());
    }
}
