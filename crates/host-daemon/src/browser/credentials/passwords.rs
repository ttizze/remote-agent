use super::{
    StoragePassword, chromium_key, chromium_payload, decrypt_chromium_payload, password_origin,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zeroize::Zeroizing;
const SERVICE: &str = "app.bex.BEX.browser-passwords";
static VAULT_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Serialize, Deserialize)]
struct Login {
    username: String,
    password: Zeroizing<String>,
}

fn options(origin: &str) -> security_framework::passwords::PasswordOptions {
    let mut options =
        security_framework::passwords::PasswordOptions::new_generic_password(SERVICE, origin);
    options.set_access_synchronized(Some(false));
    options
}

fn load(origin: &str) -> Result<Vec<Login>, String> {
    let bytes = match security_framework::passwords::generic_password(options(origin)) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(error) if error.code() == -25300 => return Ok(Vec::new()),
        Err(_) => {
            return Err("Keychain を開けません。アクセスを許可して再試行してください。".into());
        }
    };
    serde_json::from_slice(&bytes).map_err(|_| "保存パスワードを読み取れません。".into())
}

fn save(origin: &str, imported: Vec<Login>) -> Result<usize, String> {
    use security_framework::passwords::set_generic_password_options;
    let _write = VAULT_WRITES
        .lock()
        .map_err(|_| "パスワードの保存を再試行してください。")?;
    let count = imported.len();
    let mut logins = load(origin)?;
    for login in imported {
        if let Some(existing) = logins
            .iter_mut()
            .find(|existing| existing.username == login.username)
        {
            *existing = login;
        } else {
            logins.push(login);
        }
    }
    let bytes =
        Zeroizing::new(serde_json::to_vec(&logins).map_err(|_| "パスワードを保存できません。")?);
    let options = options(origin);
    set_generic_password_options(&bytes, options)
        .map_err(|_| "Keychain に保存できません。アクセスを許可して再試行してください。")?;
    Ok(count)
}

fn read_passwords(
    profile: &std::path::Path,
    password: StoragePassword,
) -> Result<(BTreeMap<String, Vec<Login>>, usize), String> {
    let mut db = rusqlite::Connection::open_with_flags(
        profile.join("Login Data"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(
        |_| "保存パスワードを開けません。取り込み元ブラウザのアクセス権を確認してください。",
    )?;
    db.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| "パスワードの読み込みを設定できません。")?;
    let transaction = db
        .transaction()
        .map_err(|_| "保存パスワードを読み取れません。")?;
    let mut statement = transaction.prepare("SELECT origin_url, signon_realm, username_value, password_value FROM logins WHERE blacklisted_by_user = 0 AND scheme = 0 ORDER BY date_created LIMIT 10001")
        .map_err(|_| "このブラウザの保存パスワード形式には対応していません。")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(|_| "保存パスワードを読み取れません。")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "保存パスワードを読み取れません。")?;
    drop(statement);
    drop(transaction);
    drop(db);
    if rows.len() > 10_000 {
        return Err("保存パスワードは10,000件まで取り込めます。".into());
    }
    let mut key = None;
    let mut result: BTreeMap<String, Vec<Login>> = BTreeMap::new();
    let mut skipped = 0;
    for (url, realm, username, encrypted) in rows {
        let Ok(origin) = password_origin(&url) else {
            skipped += 1;
            continue;
        };
        if password_origin(&realm).as_deref() != Ok(origin.as_str()) || encrypted.is_empty() {
            skipped += 1;
            continue;
        }
        let encrypted = chromium_payload(&encrypted)?;
        if key.is_none() {
            key = Some(chromium_key(&Zeroizing::new(password()?)));
        }
        let clear = decrypt_chromium_payload(encrypted, key.as_ref().unwrap())?;
        let password = Zeroizing::new(
            std::str::from_utf8(&clear)
                .map_err(|_| "パスワードの文字コードに対応していません。")?
                .to_owned(),
        );
        if password.is_empty() {
            skipped += 1;
            continue;
        }
        let logins = result.entry(origin).or_default();
        let login = Login { username, password };
        if let Some(existing) = logins
            .iter_mut()
            .find(|existing| existing.username == login.username)
        {
            *existing = login;
        } else {
            logins.push(login);
        }
    }
    Ok((result, skipped))
}

pub(super) fn import(
    profile: &std::path::Path,
    password: StoragePassword,
) -> Result<String, String> {
    let (origins, skipped) = read_passwords(profile, password)?;
    let mut saved = 0;
    for (origin, logins) in origins {
        saved += save(&origin,logins).map_err(|_|format!("{saved}件を保存した後、Keychainへの保存に失敗しました。アクセスを確認して再試行してください。"))?;
    }
    Ok(format!(
        "パスワードを{saved}件Keychainに保存しました（HTTPS以外など{skipped}件を除外）。入力はサイトごとに選択してください。"
    ))
}
pub(super) fn accounts(origin: &str) -> Result<Vec<String>, String> {
    load(origin).map(|logins| logins.into_iter().map(|login| login.username).collect())
}
pub(super) fn script(origin: &str, username: &str) -> Result<Zeroizing<String>, String> {
    let login = load(origin)?
        .into_iter()
        .find(|login| login.username == username)
        .ok_or("保存パスワードが見つかりません。")?;
    let origin = serde_json::to_string(origin).unwrap();
    let username = Zeroizing::new(serde_json::to_string(&login.username).unwrap());
    let password = Zeroizing::new(serde_json::to_string(&login.password).unwrap());
    Ok(Zeroizing::new(format!(
        "({})({origin},{},{})",
        include_str!("../password-fill.generated.js"),
        username.as_str(),
        password.as_str()
    )))
}
pub(super) fn delete(origin: &str) -> Result<String, String> {
    let _write = VAULT_WRITES
        .lock()
        .map_err(|_| "パスワードの削除を再試行してください。")?;
    security_framework::passwords::delete_generic_password_options(options(origin))
        .map_err(|_| "Keychainのパスワードを削除できません。")?;
    Ok("このサイトの保存パスワードを削除しました。".into())
}

#[cfg(test)]
mod tests {
    use super::chromium_key;
    use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};

    fn storage_secret() -> Result<Vec<u8>, String> {
        Ok(b"synthetic-storage-key".to_vec())
    }

    fn fixture() -> (tempfile::TempDir, rusqlite::Connection) {
        let root = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(root.path().join("Login Data")).unwrap();
        db.execute_batch("CREATE TABLE logins(origin_url TEXT,signon_realm TEXT,username_value TEXT,password_value BLOB,blacklisted_by_user INTEGER,scheme INTEGER,date_created INTEGER)").unwrap();
        (root, db)
    }

    fn insert(db: &rusqlite::Connection, url: &str, realm: &str, value: &str) {
        let key = chromium_key(&storage_secret().unwrap());
        let mut encrypted = b"v10".to_vec();
        encrypted.extend(
            cbc::Encryptor::<aes::Aes128>::new((&*key).into(), (&[b' '; 16]).into())
                .encrypt_padded_vec::<Pkcs7>(value.as_bytes()),
        );
        db.execute(
            "INSERT INTO logins VALUES (?1,?2,'fixture-user',?3,0,0,0)",
            rusqlite::params![url, realm, encrypted],
        )
        .unwrap();
    }

    #[test]
    fn browser_password_import_decrypts_and_keeps_accounts_scoped() {
        use super::read_passwords;
        let (root, db) = fixture();
        insert(
            &db,
            "https://example.test/login",
            "https://example.test/",
            "synthetic-original",
        );
        insert(
            &db,
            "https://example.test/other",
            "https://example.test/",
            "synthetic-updated",
        );
        insert(
            &db,
            "https://sub.example.test/login",
            "https://sub.example.test/",
            "synthetic-subdomain",
        );
        insert(
            &db,
            "http://example.test/login",
            "http://example.test/",
            "synthetic-http",
        );
        insert(
            &db,
            "https://example.test/login",
            "federation://example.test/provider",
            "synthetic-federated",
        );
        let (origins, skipped) = read_passwords(root.path(), storage_secret).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!(origins.len(), 2);
        let logins = &origins["https://example.test"];
        assert_eq!(logins.len(), 1);
        assert_eq!(logins[0].username, "fixture-user");
        assert_eq!(&*logins[0].password, "synthetic-updated");
        assert_eq!(
            &*origins["https://sub.example.test"][0].password,
            "synthetic-subdomain"
        );
    }

    #[test]
    fn browser_password_import_fails_closed_without_echoing_source_values() {
        use super::read_passwords;
        let (root, db) = fixture();
        insert(
            &db,
            "https://example.test/",
            "https://example.test/",
            "synthetic-value",
        );
        let error = read_passwords(root.path(), || Err("Access denied".into()))
            .err()
            .unwrap();
        assert!(!error.contains("synthetic-value"));
        db.execute("UPDATE logins SET password_value=x'76323000'", [])
            .unwrap();
        assert!(read_passwords(root.path(), storage_secret).is_err());
        db.execute("UPDATE logins SET origin_url='http://example.test',signon_realm='http://example.test/'",[]).unwrap();
        let (origins, skipped) = read_passwords(root.path(), || {
            panic!("ineligible sites must not request a keychain secret")
        })
        .unwrap();
        assert!(origins.is_empty());
        assert_eq!(skipped, 1);
    }
}
